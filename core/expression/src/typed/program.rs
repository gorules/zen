use crate::typed::batch::{Batch, Feeds, Origin};
use crate::typed::build::{Build, Built, Passes, Stage};
use crate::typed::exec::{Buf, Exec, Reg, Stack};
use crate::typed::hir::{DictId, Hir, Lit, NodeId, Op, Unsupported};
use crate::typed::ins::{BufDef, Rep, R};
use crate::typed::kernel::K;
use crate::typed::output::Outputs;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

type Sig = Vec<Option<u8>>;

pub(crate) struct Plan {
    id: u64,
    stages: Vec<Stage>,
    nregs: usize,
    consts: Vec<(R, Rep, Lit)>,
    defs: Vec<BufDef>,
}

pub struct Program {
    hir: Hir,
    passes: Passes,
    classes: Vec<(Origin, DictId)>,
    class_of: HashMap<NodeId, u16>,
    generic: Arc<Plan>,
    plans: RwLock<Vec<(Sig, Arc<Plan>)>>,
}

pub(crate) struct Machine {
    files: Vec<(u64, Vec<Reg>)>,
    bufs: Vec<Buf>,
    scratch: Vec<i64>,
    stack: Stack,
    sig: Vec<Option<u8>>,
}

impl Plan {
    fn new(built: Built) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let Built { stages, nregs, consts, defs } = built;
        Self { id: NEXT.fetch_add(1, Ordering::Relaxed), stages, nregs, consts, defs }
    }

    #[cfg(test)]
    pub(crate) fn dump(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "regs {} consts {}", self.nregs, self.consts.len());
        for (b, def) in self.defs.iter().enumerate() {
            let _ = writeln!(s, "buf {b} {def:?}");
        }
        for (i, stage) in self.stages.iter().enumerate() {
            let _ = writeln!(s, "stage {i} @{}", stage.scope);
            for (pc, ins) in stage.code.iter().enumerate() {
                let _ = writeln!(s, "  {pc:>3} {ins:?}");
            }
        }
        s
    }

    pub(crate) fn instructions(&self) -> usize {
        self.stages.iter().map(|s| s.code.len()).sum()
    }
}

impl Program {
    const PLANS: usize = 16;

    pub fn new(hir: Hir) -> Result<Self, Unsupported> {
        Self::with(hir, Passes::ALL)
    }

    pub(crate) fn with(hir: Hir, passes: Passes) -> Result<Self, Unsupported> {
        Outputs::new(&hir)?;
        let mut classes: Vec<(Origin, DictId)> = Vec::new();
        let mut class_of = HashMap::new();
        for (i, node) in hir.nodes.iter().enumerate() {
            let Op::Classify(a, d) = node.op else {
                continue;
            };
            let origin = match hir.nodes.get(a as usize).map(|n| &n.op) {
                Some(Op::Column(c)) => Origin::Col(*c),
                Some(Op::Field(l, f)) => Origin::Field(*l, *f),
                _ => continue,
            };
            let k = match classes.iter().position(|c| *c == (origin, d)) {
                Some(k) => k,
                None => {
                    classes.push((origin, d));
                    classes.len() - 1
                }
            };
            class_of.insert(i as NodeId, u16::try_from(k).map_err(|_| Unsupported::new("typed: classes"))?);
        }
        let width = hir.schema.columns.len() + hir.schema.lists.iter().map(|l| l.fields.len()).sum::<usize>();
        let generic = Arc::new(Plan::new(Build::plan(&hir, &vec![None; width], &class_of, passes)?));
        Ok(Self { hir, passes, classes, class_of, generic, plans: RwLock::new(Vec::new()) })
    }

    pub fn hir(&self) -> &Hir {
        &self.hir
    }

    pub fn outputs(&self) -> Outputs {
        Outputs::new(&self.hir).unwrap_or_else(|_| Outputs::empty())
    }

    pub fn run(&self, batch: &Batch, out: &mut Outputs, select: Option<&[u64]>) -> Result<(), Unsupported> {
        let feeds = Feeds::new(&self.hir, &self.classes, batch)?;
        if out.slots().len() != self.hir.outputs.len() {
            *out = Outputs::new(&self.hir)?;
        }
        out.reset(feeds.rows, |scope| feeds.len(scope));
        Machine::with(|machine| {
            feeds.sig(&self.hir, &mut machine.sig);
            let plan = self.plan(&machine.sig);
            machine.exec(&plan, &feeds, out, select);
        });
        feeds.prefaults(out, select);
        out.collect(&self.hir, &feeds, select);
        Ok(())
    }

    fn plan(&self, sig: &[Option<u8>]) -> Arc<Plan> {
        if sig.iter().all(Option::is_none) {
            return self.generic.clone();
        }
        if let Some(found) = self.plans.read().ok().and_then(|plans| plans.iter().find(|(s, _)| s == sig).map(|(_, p)| p.clone())) {
            return found;
        }
        let Ok(mut plans) = self.plans.write() else {
            return self.generic.clone();
        };
        if let Some((_, p)) = plans.iter().find(|(s, _)| s == sig) {
            return p.clone();
        }
        if plans.len() >= Self::PLANS {
            return self.generic.clone();
        }
        match Build::plan(&self.hir, sig, &self.class_of, self.passes) {
            Ok(built) => {
                let plan = Arc::new(Plan::new(built));
                plans.push((sig.to_vec(), plan.clone()));
                plan
            }
            Err(_) => self.generic.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn dump(&self, batch: &Batch) -> Result<String, Unsupported> {
        let feeds = Feeds::new(&self.hir, &self.classes, batch)?;
        let mut sig = Vec::new();
        feeds.sig(&self.hir, &mut sig);
        Ok(self.plan(&sig).dump())
    }

    pub fn instructions(&self) -> usize {
        self.generic.instructions()
    }
}

impl Machine {
    const FILES: usize = 8;

    thread_local! {
        static MACHINE: RefCell<Machine> = RefCell::new(Machine::new());
    }

    fn new() -> Self {
        Self { files: Vec::new(), bufs: Vec::new(), scratch: vec![0; 2 * Reg::LANES + 1], stack: Vec::with_capacity(16), sig: Vec::new() }
    }

    fn with<T>(f: impl FnOnce(&mut Machine) -> T) -> T {
        Self::MACHINE.with(|cell| match cell.try_borrow_mut() {
            Ok(mut machine) => f(&mut machine),
            Err(_) => f(&mut Machine::new()),
        })
    }

    fn exec(&mut self, plan: &Plan, feeds: &Feeds, out: &mut Outputs, select: Option<&[u64]>) {
        let Machine { files, bufs, scratch, stack, .. } = self;
        let at = match files.iter().position(|(id, _)| *id == plan.id) {
            Some(at) => at,
            None => {
                if files.len() >= Self::FILES {
                    files.remove(0);
                }
                let mut regs = vec![Reg::new(); plan.nregs];
                for (r, rep, lit) in &plan.consts {
                    if let Some(reg) = regs.get_mut(*r as usize) {
                        reg.fill(*rep, lit);
                    }
                }
                files.push((plan.id, regs));
                files.len() - 1
            }
        };
        let regs = &mut files[at].1;
        if bufs.len() < plan.defs.len() {
            bufs.resize_with(plan.defs.len(), Buf::default);
        }
        for (buf, def) in bufs.iter_mut().zip(&plan.defs) {
            let len = feeds.len(def.scope);
            buf.sized(len);
            if let Some((list, _, _, empty)) = def.fold {
                buf.mant.fill(0);
                buf.scale.fill(0);
                buf.codes.fill(0);
                buf.bits.fill(0);
                buf.fault.fill(0);
                buf.valid.fill(u64::MAX);
                if let Some(null) = feeds.lists.get(list as usize).and_then(|l| l.null.as_ref()).filter(|_| !empty) {
                    for (f, n) in buf.fault.iter_mut().zip(null) {
                        *f |= n;
                    }
                }
            }
        }
        for (at, stage) in plan.stages.iter().enumerate() {
            let len = feeds.len(stage.scope);
            let mut start = 0;
            while start < len {
                let n = (len - start).min(Reg::LANES);
                let exec = Exec { feeds, select, start, n, words: K::words(n) };
                exec.run(&stage.code, regs, bufs, scratch, stack, out);
                start += Reg::LANES;
            }
            for (buf, def) in bufs.iter_mut().zip(&plan.defs) {
                if let (Some((_, fold, rep, _)), true) = (def.fold, def.stage == at) {
                    buf.finish(fold, rep);
                }
            }
        }
    }
}

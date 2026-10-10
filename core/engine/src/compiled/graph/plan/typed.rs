use super::view::{Expr, Masks, M, Ref, Shape, SlotId};
use super::{Builder, LaneOp, Op, Program, SlotInfo, Source, TableMode, TableOp};
use crate::compiled::lowered::Exports;
use crate::compiled::shred::Seen;
use crate::compiled::graph::table::Table;
use crate::compiled::graph::{Body, CompiledGraph, Kind};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use zen_expression::lane::{Binding as Site, Fusion};
use zen_expression::typed::{Class, Lowering, Program as Typed, Resolve, Resolved};

pub(crate) enum Target {
    Slot(SlotId),
    Choice,
}

pub(crate) type Fields = Vec<(Arc<str>, Class)>;

pub(crate) type Lister<'g> = &'g dyn Fn(SlotId, &[&str]) -> Option<Fields>;

pub(crate) struct TypedOp {
    pub program: Typed,
    pub exports: Exports,
    pub columns: Vec<Ref>,
    pub lists: Vec<(Ref, Vec<Arc<str>>)>,
    pub targets: Vec<Vec<Target>>,
    pub cost: Cost,
    pub sparse: AtomicU32,
    pub shared: Vec<bool>,
}

pub(crate) enum Piece {
    Unit,
    Typed(Box<TypedOp>, usize),
    Deferred(Box<Deferred>, usize),
}

type Variant = (Vec<Seen>, Arc<[Piece]>);

pub(crate) struct Deferred {
    pub keys: Vec<SlotId>,
    variants: RwLock<Vec<Variant>>,
}

pub(crate) struct Grouping<'g> {
    pub graph: &'g CompiledGraph,
    pub masks: &'g Masks,
    pub slots: &'g [SlotInfo],
    pub seen: &'g [(SlotId, Seen)],
    pub listing: Option<Lister<'g>>,
}

enum Attempt {
    Typed(Box<TypedOp>, usize),
    Deferred,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Path {
    Typed,
    Lane,
}

pub(crate) struct Cost {
    calls: AtomicU32,
    typed: AtomicU64,
    lane: AtomicU64,
}

struct Probe<'p> {
    masks: &'p Masks,
    slots: &'p [SlotInfo],
    seen: &'p [(SlotId, Seen)],
    listing: Option<Lister<'p>>,
    lists: Vec<(String, Ref)>,
    deferred: bool,
    reads: Vec<(Arc<str>, Option<Ref>)>,
    outputs: Vec<(SlotId, Vec<M>, usize)>,
    produced: Vec<M>,
    columns: Vec<(String, Ref)>,
}

impl TypedOp {
    pub const SPARSE: u32 = 16;
    pub const LONG: usize = 24;

    fn new(program: Typed, columns: Vec<Ref>, lists: Vec<(Ref, Vec<Arc<str>>)>, targets: Vec<Vec<Target>>) -> Self {
        Self {
            exports: Exports::of(program.hir()),
            program,
            columns,
            lists,
            targets,
            cost: Cost::new(),
            sparse: AtomicU32::new(0),
            shared: Vec::new(),
        }
    }
}

impl Deferred {
    const VARIANTS: usize = 8;

    fn new(keys: Vec<SlotId>) -> Self {
        Self { keys, variants: RwLock::new(Vec::new()) }
    }

    pub fn variant(&self, seen: &[Seen], units: usize, build: impl FnOnce() -> Vec<Piece>) -> Arc<[Piece]> {
        if let Some(found) = self.variants.read().ok().and_then(|variants| variants.iter().find(|(key, _)| key == seen).map(|(_, pieces)| pieces.clone())) {
            return found;
        }
        let Ok(mut variants) = self.variants.write() else {
            return (0..units).map(|_| Piece::Unit).collect();
        };
        if let Some((_, pieces)) = variants.iter().find(|(key, _)| key == seen) {
            return pieces.clone();
        }
        let pieces: Arc<[Piece]> = match variants.len() < Self::VARIANTS {
            true => build().into(),
            false => (0..units).map(|_| Piece::Unit).collect(),
        };
        variants.push((seen.to_vec(), pieces.clone()));
        pieces
    }
}

impl Cost {
    const WARM: u32 = 2;
    const PERIOD: u32 = 64;

    fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
            typed: AtomicU64::new(u64::MAX),
            lane: AtomicU64::new(u64::MAX),
        }
    }

    pub fn path(&self) -> (Path, bool) {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if call < Self::WARM {
            return (Path::Typed, false);
        }
        if call < 2 * Self::WARM {
            return (Path::Lane, false);
        }
        let (faster, slower) = match self.typed.load(Ordering::Relaxed) <= self.lane.load(Ordering::Relaxed) {
            true => (Path::Typed, Path::Lane),
            false => (Path::Lane, Path::Typed),
        };
        match call % Self::PERIOD {
            0 => (slower, true),
            1 => (faster, true),
            _ => (faster, false),
        }
    }

    pub fn record(&self, (path, fresh): (Path, bool), elapsed: Duration, rows: usize) {
        let per = u64::try_from(elapsed.as_nanos().saturating_mul(1024) / rows.max(1) as u128).unwrap_or(u64::MAX - 1);
        let cell = match path {
            Path::Typed => &self.typed,
            Path::Lane => &self.lane,
        };
        match fresh {
            true => cell.store(per, Ordering::Relaxed),
            false => {
                cell.fetch_min(per, Ordering::Relaxed);
            }
        }
    }
}

impl Resolve for Probe<'_> {
    fn path(&mut self, path: &str) -> Resolved {
        let Some((_, read)) = self.reads.iter().find(|(key, _)| key.as_ref() == path) else {
            return Resolved::Unsupported;
        };
        let Some(read) = *read else {
            return Resolved::Absent;
        };
        if let Some((_, allowed, at)) = self.outputs.iter().find(|(slot, _, _)| *slot == read.slot) {
            return match allowed.contains(&read.present) || self.covers(read.present, read.slot, allowed) {
                true => Resolved::Output(*at),
                false => Resolved::Unsupported,
            };
        }
        if self.depends(read.present) {
            return Resolved::Unsupported;
        }
        let class = match self.slots.get(read.slot as usize).map(|info| info.shape) {
            Some(Shape::Num) => Class::Num,
            Some(Shape::Bool) => Class::Bool,
            Some(Shape::Text) => Class::Text,
            Some(Shape::Null) => return Resolved::Absent,
            Some(Shape::Dyn) => match self.seen.iter().find(|(slot, _)| *slot == read.slot).map(|(_, seen)| *seen) {
                Some(Seen::Num) => Class::Num,
                Some(Seen::Bool) => Class::Bool,
                Some(Seen::Text) => Class::Text,
                Some(Seen::Null) => return Resolved::Absent,
                Some(Seen::List) => return self.listed(read),
                Some(Seen::Mixed) => return Resolved::Unsupported,
                None => {
                    self.deferred = true;
                    return Resolved::Unsupported;
                }
            },
            _ => return Resolved::Unsupported,
        };
        let key = format!("#{}:{:?}", read.slot, read.present);
        if !self.columns.iter().any(|(k, _)| *k == key) {
            self.columns.push((key.clone(), read));
        }
        Resolved::Keyed(class, Arc::from(key))
    }

    fn alias(&mut self, _field: &str) -> Resolved {
        Resolved::Unsupported
    }

    fn nested(&mut self, chain: &[&str]) -> Resolved {
        let (Some((key, rest)), Some(listing)) = (chain.split_first(), self.listing) else {
            return Resolved::Unsupported;
        };
        let Some((_, read)) = self.lists.iter().find(|(k, _)| k.as_str() == *key) else {
            return Resolved::Unsupported;
        };
        match listing(read.slot, rest) {
            Some(fields) => Resolved::List { fields },
            None => Resolved::Unsupported,
        }
    }
}

impl Probe<'_> {
    const ATOMS: usize = 12;

    fn listed(&mut self, read: Ref) -> Resolved {
        let Some(fields) = self.listing.and_then(|listing| listing(read.slot, &[])) else {
            return Resolved::Unsupported;
        };
        let key = format!("#{}:{:?}", read.slot, read.present);
        if !self.lists.iter().any(|(k, _)| *k == key) {
            self.lists.push((key.clone(), read));
        }
        Resolved::KeyedList(Arc::from(key), fields)
    }

    fn covers(&self, m: M, slot: SlotId, implied: &[M]) -> bool {
        let mut atoms: Vec<M> = Vec::new();
        if !self.atoms(m, &mut atoms) {
            return false;
        }
        let given = |atom: &M| {
            implied.contains(atom)
                || matches!(atom, M::Id(id) if matches!(self.masks.exprs.get(*id as usize), Some(Expr::Valued(s) | Expr::Validity(s)) if *s == slot))
        };
        let free: Vec<M> = atoms.iter().copied().filter(|atom| !given(atom)).collect();
        if free.len() > Self::ATOMS {
            return false;
        }
        (0..1u32 << free.len()).all(|bits| {
            let value = |atom: M| match free.iter().position(|f| *f == atom) {
                Some(at) => bits >> at & 1 == 1,
                None => true,
            };
            self.holds(m, &value)
        })
    }

    fn atoms(&self, m: M, out: &mut Vec<M>) -> bool {
        let M::Id(id) = m else {
            return true;
        };
        match self.masks.exprs.get(id as usize) {
            Some(Expr::And(a, b) | Expr::Or(a, b)) => self.atoms(*a, out) && self.atoms(*b, out),
            Some(Expr::Not(a)) => self.atoms(*a, out),
            Some(_) => {
                if !out.contains(&m) {
                    out.push(m);
                }
                true
            }
            None => false,
        }
    }

    fn holds(&self, m: M, value: &dyn Fn(M) -> bool) -> bool {
        let M::Id(id) = m else {
            return m == M::All;
        };
        match self.masks.exprs.get(id as usize) {
            Some(Expr::And(a, b)) => self.holds(*a, value) && self.holds(*b, value),
            Some(Expr::Or(a, b)) => self.holds(*a, value) || self.holds(*b, value),
            Some(Expr::Not(a)) => !self.holds(*a, value),
            _ => value(m),
        }
    }

    fn depends(&self, m: M) -> bool {
        let M::Id(id) = m else {
            return false;
        };
        if self.produced.contains(&m) {
            return true;
        }
        match self.masks.exprs.get(id as usize) {
            Some(Expr::Validity(slot) | Expr::Valued(slot)) => self.outputs.iter().any(|(s, _, _)| s == slot),
            Some(Expr::And(a, b) | Expr::Or(a, b)) => self.depends(*a) || self.depends(*b),
            Some(Expr::Not(a)) => self.depends(*a),
            Some(Expr::Produced(_)) => false,
            None => true,
        }
    }
}

impl Builder<'_> {
    pub(super) fn typed(&mut self) {
        if !Exports::enabled() {
            return;
        }
        let ops = std::mem::take(&mut self.catalog.ops);
        let mut grouped = Vec::with_capacity(ops.len());
        let mut run: Vec<Op> = Vec::new();
        for op in ops {
            match self.grouping().eligible(&op) {
                true => run.push(op),
                false => {
                    self.flush(&mut run, &mut grouped);
                    grouped.push(op);
                }
            }
        }
        self.flush(&mut run, &mut grouped);
        let texts: Vec<SlotId> = grouped
            .iter()
            .filter_map(|op| match op {
                Op::Typed { op, .. } => Some(op.columns.iter().map(|read| read.slot)),
                _ => None,
            })
            .flatten()
            .filter(|slot| matches!(self.catalog.slots.get(*slot as usize), Some(SlotInfo { shape: Shape::Text, source: Source::Input(_) })))
            .collect();
        for op in &mut grouped {
            if let Op::Typed { op, .. } = op {
                op.shared = op.columns.iter().map(|read| texts.iter().filter(|slot| **slot == read.slot).count() > 1).collect();
            }
        }
        self.catalog.ops = grouped;
    }

    fn grouping(&self) -> Grouping<'_> {
        Grouping {
            graph: self.graph,
            masks: &self.masks,
            slots: &self.catalog.slots,
            seen: &[],
            listing: None,
        }
    }

    fn flush(&self, run: &mut Vec<Op>, out: &mut Vec<Op>) {
        if run.is_empty() {
            return;
        }
        let pieces = self.grouping().pieces(run);
        let mut units = std::mem::take(run).into_iter();
        for piece in pieces {
            match piece {
                Piece::Unit => out.extend(units.next()),
                Piece::Typed(op, n) => out.push(Op::Typed { op, units: units.by_ref().take(n).collect() }),
                Piece::Deferred(op, n) => out.push(Op::Deferred { op, units: units.by_ref().take(n).collect() }),
            }
        }
    }
}

impl Grouping<'_> {
    pub fn pieces(&self, ops: &[Op]) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut at = 0;
        while let Some(rest) = ops.get(at..).filter(|rest| !rest.is_empty()) {
            match self.lower(rest) {
                Attempt::Typed(_, n) if rest.iter().take(n).all(|op| matches!(op, Op::Lane(lane) if lane.uniform)) => {
                    pieces.extend((0..n).map(|_| Piece::Unit));
                    at += n;
                }
                Attempt::Typed(op, n) => {
                    pieces.push(Piece::Typed(op, n));
                    at += n;
                }
                Attempt::Deferred => {
                    pieces.push(Piece::Deferred(Box::new(Deferred::new(self.keys(rest))), rest.len()));
                    at = ops.len();
                }
                Attempt::Failed => {
                    pieces.push(Piece::Unit);
                    at += 1;
                }
            }
        }
        pieces
    }

    fn keys(&self, ops: &[Op]) -> Vec<SlotId> {
        let mut produced: Vec<SlotId> = Vec::new();
        let mut keys: Vec<SlotId> = Vec::new();
        for op in ops {
            let (reads, outs): (Vec<SlotId>, &[SlotId]) = match op {
                Op::Lane(lane) => (lane.reads.iter().map(|(_, read)| read.slot).collect(), &lane.outs),
                Op::Table(table) => (table.reads.iter().filter_map(|(_, read)| read.map(|read| read.slot)).collect(), &table.outs),
                _ => (Vec::new(), &[]),
            };
            for slot in reads {
                let dynamic = matches!(self.slots.get(slot as usize), Some(SlotInfo { shape: Shape::Dyn, .. }));
                if dynamic && !produced.contains(&slot) && !keys.contains(&slot) {
                    keys.push(slot);
                }
            }
            produced.extend_from_slice(outs);
        }
        keys
    }

    fn eligible(&self, op: &Op) -> bool {
        match op {
            Op::Lane(lane) => matches!(lane.program, Program::Expression) && !lane.sources.is_empty() && lane.sources.len() == lane.outs.len(),
            Op::Table(table) => table.mode == TableMode::First && self.tabled(table).is_some_and(|t| t.spec.is_some()),
            _ => false,
        }
    }

    fn tabled(&self, op: &TableOp) -> Option<&Table> {
        match &self.graph.step_at(op.node)?.kind {
            Kind::Node { body: Body::Table(table), .. } => Some(table),
            _ => None,
        }
    }

    fn sites(&self, op: &LaneOp) -> Option<Vec<(Arc<str>, Option<Ref>)>> {
        let program = match &op.own {
            Some(own) => &own.program,
            None => match &self.graph.step_at(op.node)?.kind {
                Kind::Node { body: Body::Expression { program, .. }, .. } => program.as_ref(),
                _ => return None,
            },
        };
        let mut reads: Vec<(Arc<str>, Option<Ref>)> = op.reads.iter().map(|(key, read)| (key.clone(), Some(*read))).collect();
        for (key, site) in program.program().site_keys.iter().zip(&op.bound) {
            if let (Some(key), Site::Absent) = (key, site) {
                reads.push((Arc::from(key.as_str()), None));
            }
        }
        Some(reads)
    }

    fn lower(&self, ops: &[Op]) -> Attempt {
        let mut limit = ops.len();
        loop {
            let mut lowering = Lowering::graph();
            let mut probe = Probe {
                masks: self.masks,
                slots: self.slots,
                seen: self.seen,
                listing: self.listing,
                lists: Vec::new(),
                deferred: false,
                reads: Vec::new(),
                outputs: Vec::new(),
                produced: Vec::new(),
                columns: Vec::new(),
            };
            let mut targets: Vec<Vec<Target>> = Vec::new();
            let mut kept: Vec<bool> = Vec::new();
            for op in &ops[..limit] {
                let base = lowering.outputs();
                probe.deferred = false;
                let lowered = match op {
                    Op::Lane(lane) if self.eligible(op) => {
                        let Some(reads) = self.sites(lane) else {
                            break;
                        };
                        probe.reads = reads;
                        let entries: Vec<Fusion> = lane
                            .sources
                            .iter()
                            .map(|(key, source)| Fusion::Output {
                                key: key.to_string(),
                                source: source.to_string(),
                            })
                            .collect();
                        lowering.unit(&entries, &mut probe)
                    }
                    Op::Table(table) if self.eligible(op) => {
                        let Some(spec) = self.tabled(table).and_then(|t| t.spec.as_ref()) else {
                            break;
                        };
                        probe.reads = table.reads.clone();
                        lowering.table(spec, &mut probe)
                    }
                    _ => break,
                };
                if lowered.is_err() {
                    if lowering.units() == 0 && probe.deferred {
                        return Attempt::Deferred;
                    }
                    break;
                }
                let unit: Vec<Option<Target>> = match op {
                    Op::Lane(lane) => {
                        for (at, slot) in lane.outs.iter().enumerate() {
                            probe.outputs.push((*slot, vec![M::All], base + at));
                        }
                        lane.outs.iter().map(|slot| Some(Target::Slot(*slot))).collect()
                    }
                    Op::Table(table) => {
                        for (at, (slot, present)) in table.outs.iter().zip(&table.present).enumerate() {
                            probe.outputs.push((*slot, vec![M::All, *present, table.matched], base + at));
                        }
                        probe.produced.extend(table.present.iter().copied());
                        probe.produced.push(table.matched);
                        let literal = self.tabled(table).is_some_and(Table::literal);
                        table
                            .outs
                            .iter()
                            .map(|slot| (!literal).then_some(Target::Slot(*slot)))
                            .chain(std::iter::once(Some(Target::Choice)))
                            .collect()
                    }
                    _ => Vec::new(),
                };
                if lowering.outputs() != base + unit.len() {
                    return Attempt::Failed;
                }
                kept.extend(unit.iter().map(Option::is_some));
                targets.push(unit.into_iter().flatten().collect());
            }
            let consumed = lowering.units();
            if consumed == 0 {
                return Attempt::Failed;
            }
            kept.resize(lowering.outputs(), true);
            lowering.retain(|at| kept.get(at).copied().unwrap_or(true));
            match Typed::new(lowering.finish()) {
                Ok(program) => {
                    let schema = &program.hir().schema;
                    let columns = schema
                        .columns
                        .iter()
                        .map(|column| probe.columns.iter().find(|(key, _)| *key == column.path).map(|(_, read)| *read))
                        .collect::<Option<Vec<Ref>>>();
                    let lists = (0..schema.lists.len())
                        .map(|at| {
                            let chain = schema.chain(at as u16);
                            let (key, rest) = chain.split_first()?;
                            let (_, read) = probe.lists.iter().find(|(k, _)| k.as_str() == *key)?;
                            Some((*read, rest.iter().map(|field| Arc::from(*field)).collect()))
                        })
                        .collect::<Option<Vec<(Ref, Vec<Arc<str>>)>>>();
                    return match (columns, lists) {
                        (Some(columns), Some(lists)) => Attempt::Typed(Box::new(TypedOp::new(program, columns, lists, targets)), consumed),
                        _ => Attempt::Failed,
                    };
                }
                Err(_) if consumed == 1 => return Attempt::Failed,
                Err(_) => limit = consumed - 1,
            }
        }
    }
}

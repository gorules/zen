use super::view::{Expr, Masks, M, Ref, Shape, SlotId};
use super::{Builder, LaneOp, Op, Program, SlotInfo, Source, TableMode, TableOp};
use crate::compiled::core::exec::Dedup;
use super::census::Census;
use crate::compiled::core::group::{Attempt, Built, Prefix, Step};
use crate::compiled::core::switch::Cost;
use crate::compiled::core::variants::Variants;
use crate::compiled::lowered::Exports;
use crate::compiled::shred::Seen;
use crate::compiled::graph::table::Table;
use crate::compiled::graph::{Body, CompiledGraph, Kind};
use std::sync::Arc;
use zen_expression::lane::{Binding as Site, Fusion};
use zen_expression::typed::{Class, Lowering, Program as Typed, Resolve, Resolved, TableSpec};

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
    pub dedup: Dedup,
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
    variants: Variants<Variant>,
}

pub(crate) struct Grouping<'g> {
    pub graph: &'g CompiledGraph,
    pub masks: &'g Masks,
    pub slots: &'g [SlotInfo],
    pub seen: &'g [(SlotId, Seen)],
    pub listing: Option<Lister<'g>>,
}

struct Probe<'p> {
    masks: &'p Masks,
    slots: &'p [SlotInfo],
    seen: &'p [(SlotId, Seen)],
    listing: Option<Lister<'p>>,
    lists: Vec<(String, Ref)>,
    deferred: bool,
    novel: bool,
    reads: Vec<(Arc<str>, Option<Ref>)>,
    outputs: Vec<(SlotId, Vec<M>, usize)>,
    produced: Vec<M>,
    columns: Vec<(String, Ref)>,
}

impl TypedOp {
    fn new(program: Typed, columns: Vec<Ref>, lists: Vec<(Ref, Vec<Arc<str>>)>, targets: Vec<Vec<Target>>) -> Self {
        Self {
            exports: Exports::of(program.hir()),
            program,
            columns,
            lists,
            targets,
            cost: Cost::new(),
            dedup: Dedup::new(),
            shared: Vec::new(),
        }
    }
}

impl Deferred {
    const VARIANTS: usize = 8;

    fn new(keys: Vec<SlotId>) -> Self {
        Self { keys, variants: Variants::new(Self::VARIANTS) }
    }

    pub fn variant(&self, seen: &[Seen], units: usize, build: impl FnOnce() -> Vec<Piece>) -> Arc<[Piece]> {
        let plain = || -> Arc<[Piece]> { (0..units).map(|_| Piece::Unit).collect() };
        self.variants
            .get_or_build(|(key, pieces)| (key == seen).then(|| pieces.clone()), || (seen.to_vec(), build().into()), || (seen.to_vec(), plain()))
            .unwrap_or_else(plain)
    }
}

impl Resolve for Probe<'_> {
    fn path(&mut self, path: &str) -> Resolved {
        let Some((_, read)) = self.reads.iter().find(|(key, _)| key.as_ref() == path) else {
            Census::note("unbound: ");
            return Resolved::Unsupported;
        };
        let Some(read) = *read else {
            return Resolved::Absent;
        };
        if let Some((_, allowed, at)) = self.outputs.iter().find(|(slot, _, _)| *slot == read.slot) {
            return match allowed.contains(&read.present) || self.covers(read.present, read.slot, allowed) {
                true => Resolved::Output(*at),
                false => {
                    Census::note("output mask: ");
                    Resolved::Unsupported
                }
            };
        }
        if self.depends(read.present) {
            Census::note("depends: ");
            return Resolved::Unsupported;
        }
        let slot = self.slots.get(read.slot as usize);
        let class = match slot.map(|info| info.shape) {
            Some(Shape::Num) => Class::Num,
            Some(Shape::Bool) => Class::Bool,
            Some(Shape::Text) => Class::Text,
            Some(Shape::Null) => return Resolved::Absent,
            Some(shape @ (Shape::Dyn | Shape::Unknown)) if shape == Shape::Dyn || slot.is_some_and(|info| matches!(info.source, Source::Input(_))) => match self.observed(read.slot, shape) {
                Some(Seen::Num) => Class::Num,
                Some(Seen::Bool) => Class::Bool,
                Some(Seen::Text) => Class::Text,
                Some(Seen::Null) => return Resolved::Absent,
                Some(Seen::List) => return self.listed(read),
                Some(Seen::Mixed) => {
                    Census::note("mixed: ");
                    return Resolved::Unsupported;
                }
                None => {
                    self.deferred = true;
                    return Resolved::Unsupported;
                }
            },
            Some(Shape::List) => {
                return match self.listing {
                    Some(_) => match self.listed(read) {
                        Resolved::KeyedList(key, fields) if fields.iter().all(|(name, _)| name.is_empty()) => {
                            self.novel = true;
                            Resolved::KeyedList(key, fields)
                        }
                        _ => {
                            Census::note("object list: ");
                            Resolved::Unsupported
                        }
                    },
                    None => {
                        self.deferred = true;
                        Census::note("list: ");
                        Resolved::Unsupported
                    }
                };
            }
            other => {
                Census::note(&format!("shape {other:?}: "));
                return Resolved::Unsupported;
            }
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

    fn novel(&mut self) -> bool {
        std::mem::take(&mut self.novel)
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

    fn observed(&mut self, slot: SlotId, shape: Shape) -> Option<Seen> {
        let seen = self.seen.iter().find(|(at, _)| *at == slot).map(|(_, seen)| *seen);
        self.novel |= shape == Shape::Unknown && seen.is_some();
        seen
    }

    fn listed(&mut self, read: Ref) -> Resolved {
        let Some(fields) = self.listing.and_then(|listing| listing(read.slot, &[])) else {
            Census::note("unlisted: ");
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
                    Census::reason(|| Grouping::ineligible(&op));
                    Census::refuse(&op, self.graph);
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
                    for op in rest.iter().take(n) {
                        Census::reason(|| "uniform".into());
                        Census::refuse(op, self.graph);
                    }
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
                    Census::refuse(&rest[0], self.graph);
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
                let dynamic = matches!(self.slots.get(slot as usize), Some(SlotInfo { shape: Shape::Dyn, .. } | SlotInfo { shape: Shape::Unknown, source: Source::Input(_) }));
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

    fn lower(&self, ops: &[Op]) -> Attempt<Box<TypedOp>> {
        Prefix::lower(
            ops.len(),
            || {
                let probe = Probe {
                    masks: self.masks,
                    slots: self.slots,
                    seen: self.seen,
                    listing: self.listing,
                    lists: Vec::new(),
                    deferred: false,
                    novel: false,
                    reads: Vec::new(),
                    outputs: Vec::new(),
                    produced: Vec::new(),
                    columns: Vec::new(),
                };
                (Lowering::graph(), probe, Vec::new(), Vec::new())
            },
            |(lowering, probe, targets, kept), at| self.unit(&ops[at], lowering, probe, targets, kept),
            |(lowering, probe, targets, kept)| self.build(lowering, probe, targets, kept),
        )
    }

    fn unit(&self, op: &Op, lowering: &mut Lowering, probe: &mut Probe, targets: &mut Vec<Vec<Target>>, kept: &mut Vec<bool>) -> Step<Attempt<Box<TypedOp>>> {
        let base = lowering.outputs();
        probe.deferred = false;
        probe.novel = false;
        let mut blanked: Vec<bool> = Vec::new();
        Census::fresh();
        let lowered = match op {
            Op::Lane(lane) if self.eligible(op) => {
                let Some(reads) = self.sites(lane) else {
                    return Step::Stop;
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
                lowering.unit(&entries, probe)
            }
            Op::Table(table) if self.eligible(op) => {
                let Some((found, spec)) = self.tabled(table).and_then(|t| Some((t, t.spec.as_ref()?))) else {
                    return Step::Stop;
                };
                probe.reads = table.reads.clone();
                match Self::blank(found, spec) {
                    Some((blank, columns)) => {
                        probe.novel = true;
                        blanked = columns;
                        lowering.table(&blank, probe)
                    }
                    None => lowering.table(spec, probe),
                }
            }
            _ => {
                Census::reason(|| Self::ineligible(op));
                return Step::Stop;
            }
        };
        if let Err(e) = &lowered {
            Census::reason(|| e.0.clone());
            return match lowering.units() == 0 && probe.deferred {
                true => Step::Done(Attempt::Deferred),
                false => Step::Stop,
            };
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
                    if !blanked.get(at).copied().unwrap_or(false) {
                        probe.outputs.push((*slot, vec![M::All, *present, table.matched], base + at));
                    }
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
            Census::reason(|| "output count".into());
            return Step::Done(Attempt::Failed);
        }
        kept.extend(unit.iter().map(Option::is_some));
        targets.push(unit.into_iter().flatten().collect());
        Step::Lowered
    }

    fn build(&self, mut lowering: Lowering, probe: Probe, targets: Vec<Vec<Target>>, mut kept: Vec<bool>) -> Built<Attempt<Box<TypedOp>>> {
        let consumed = lowering.units();
        if consumed == 0 {
            return Built::Done(Attempt::Failed);
        }
        kept.resize(lowering.outputs(), true);
        lowering.retain(|at| kept.get(at).copied().unwrap_or(true));
        let program = match Typed::new(lowering.finish()) {
            Ok(program) => program,
            Err(e) => {
                Census::reason(|| format!("build: {}", e.0));
                return Built::Shrink {
                    consumed,
                    fallback: Attempt::Failed,
                };
            }
        };
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
        Built::Done(match (columns, lists) {
            (Some(columns), Some(lists)) => Attempt::Typed(Box::new(TypedOp::new(program, columns, lists, targets)), consumed),
            _ => {
                Census::reason(|| "columns unresolved".into());
                Attempt::Failed
            }
        })
    }

    fn blank(table: &Table, spec: &TableSpec) -> Option<(TableSpec, Vec<bool>)> {
        if !table.literal() {
            return None;
        }
        let composite = |value: &Option<String>| value.as_deref().is_some_and(|v| v.trim_start().starts_with(['[', '{']));
        let columns: Vec<bool> = (0..spec.outputs.len()).map(|at| spec.rules.iter().any(|rule| rule.values.get(at).is_some_and(composite))).collect();
        if !columns.contains(&true) {
            return None;
        }
        let mut blank = spec.clone();
        for rule in &mut blank.rules {
            for (value, blanked) in rule.values.iter_mut().zip(&columns) {
                if *blanked {
                    *value = None;
                }
            }
        }
        Some((blank, columns))
    }

    fn ineligible(op: &Op) -> String {
        match op {
            Op::Lane(lane) if !matches!(lane.program, Program::Expression) => "ineligible: switch/field program".into(),
            Op::Lane(_) => "ineligible: sources".into(),
            Op::Table(table) if table.mode != TableMode::First => "ineligible: collect/rows table".into(),
            Op::Table(_) => "ineligible: table spec".into(),
            _ => "ineligible".into(),
        }
    }
}

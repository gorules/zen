use super::view::{Expr, MaskId, M, Node, Ref, SlotId};
use crate::compiled::data::{Data, Mask};
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;
use super::{Cond, LaneOp, Layout, Op, Plan, Program, SegEnd, SegPlan, Source, TableMode, TableOp};
use crate::compiled::bound::{Bound, Outs};
use crate::compiled::typed::{Array, Bits, ColumnBuilder, Dict, Leaf, Literal, Store as Buffer};
use crate::compiled::{Body, ColumnarOutput, CompiledGraph, Condition, Kind, OutputColumn};
use crate::model::GraphContent;
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Binding as Bind, Column, Columns, Dictionary, LaneProgram, LaneRunner, Values};

struct Failure(Rc<str>);

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, f)
    }
}

impl std::error::Error for Failure {}

struct Shaped<'a> {
    leaf: Option<(Leaf<'a>, Rc<[u64]>)>,
    obj: Option<Rc<[u64]>>,
    fields: Vec<(Symbol, Shaped<'a>)>,
}

type Entries<'g> = Option<&'g [(Arc<str>, Arc<str>)]>;
type Spread = (Rc<[u64]>, Rc<[u64]>);
type View<'v> = (Column<'v>, bool, Option<&'v Rc<[u64]>>, Option<&'v Rc<Dict>>);
type Origin<'s, 'a> = Option<(&'s Leaf<'a>, &'s Option<Rc<[u64]>>)>;

type Group<'a> = (Option<Rc<[usize]>>, Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)>);

pub(crate) struct Store<'a> {
    rows: usize,
    words: usize,
    input: &'a Columns<'a>,
    slots: Vec<Option<Leaf<'a>>>,
    inputs: Vec<Option<(Column<'a>, std::cell::OnceCell<Leaf<'a>>)>>,
    arena: Vec<u64>,
    offsets: Vec<u32>,
    masked: Vec<(SlotId, M, Leaf<'a>)>,
    shared: Vec<(usize, Rc<[u64]>)>,
    ones: Rc<[u64]>,
    errors: Vec<Option<Box<EvaluationError>>>,
    failed: usize,
    groups: Vec<Group<'a>>,
    hints: Vec<(SlotId, Rc<[i32]>, usize)>,
}

impl Drop for Store<'_> {
    fn drop(&mut self) {
        let (arena, offsets) = (std::mem::take(&mut self.arena), std::mem::take(&mut self.offsets));
        Store::POOL.with_borrow_mut(|pool| *pool = (arena, offsets));
    }
}

impl<'a> Store<'a> {
    const NONE: u32 = u32::MAX;
    const ONES: usize = 0;

    thread_local! {
        static POOL: std::cell::RefCell<(Vec<u64>, Vec<u32>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
    }

    fn new(seg: &SegPlan, input: &'a Columns<'a>) -> Self {
        let rows = input.rows;
        let words = rows.div_ceil(64);
        let slots = vec![None; seg.slots.len()];
        let inputs = seg
            .slots
            .iter()
            .map(|info| match info.source {
                Source::Input(index) => input.columns.get(index).map(|(_, column)| (*column, std::cell::OnceCell::new())),
                Source::Op => None,
            })
            .collect();
        let (mut arena, mut offsets) = Self::POOL.with_borrow_mut(std::mem::take);
        let ones: Rc<[u64]> = Bits::ones(rows).into();
        arena.clear();
        arena.extend_from_slice(&ones);
        arena.resize(words * 2, 0);
        offsets.clear();
        offsets.resize(seg.masks.exprs.len(), Self::NONE);
        Self {
            rows,
            words,
            input,
            slots,
            inputs,
            arena,
            offsets,
            masked: Vec::new(),
            shared: Vec::new(),
            ones,
            errors: (0..rows).map(|_| None).collect(),
            failed: 0,
            groups: Vec::new(),
            hints: Vec::new(),
        }
    }

    fn enter(&mut self, seg: &SegPlan) {
        self.slots.resize(seg.slots.len(), None);
        self.offsets.resize(seg.masks.exprs.len(), Self::NONE);
    }

    fn rewind(&mut self, seg: &SegPlan) {
        let (slots, masks) = (seg.slots.len(), seg.masks.exprs.len());
        self.slots.truncate(slots);
        self.offsets.truncate(masks);
        self.masked.retain(|(slot, m, _)| {
            (*slot as usize) < slots
                && match m {
                    M::Id(id) => (*id as usize) < masks,
                    _ => true,
                }
        });
    }

    fn leaf(&self, slot: SlotId) -> Leaf<'a> {
        if let Some(Some(leaf)) = self.slots.get(slot as usize) {
            return leaf.clone();
        }
        match self.inputs.get(slot as usize) {
            Some(Some((column, leaf))) => leaf.get_or_init(|| CompiledGraph::input_leaf(*column, self.rows)).clone(),
            _ => Leaf::nulls(self.rows),
        }
    }

    fn bits(&self, at: usize) -> &[u64] {
        &self.arena[at..at + self.words]
    }

    fn mask(&mut self, seg: &SegPlan, m: M) -> Rc<[u64]> {
        match m {
            M::All => self.ones.clone(),
            m => {
                let at = self.region(seg, m);
                if let Some((_, shared)) = self.shared.iter().find(|(offset, _)| *offset == at) {
                    return shared.clone();
                }
                let bits: Rc<[u64]> = self.bits(at).into();
                self.shared.push((at, bits.clone()));
                bits
            }
        }
    }

    fn region(&mut self, seg: &SegPlan, m: M) -> usize {
        match m {
            M::All => Self::ONES,
            M::None => self.words,
            M::Id(id) => self.expr(seg, id),
        }
    }

    fn push(&mut self, bits: &[u64]) -> usize {
        let at = self.arena.len();
        self.arena.extend_from_slice(&bits[..self.words.min(bits.len())]);
        self.arena.resize(at + self.words, 0);
        at
    }

    fn expr(&mut self, seg: &SegPlan, id: MaskId) -> usize {
        if let Some(&at) = self.offsets.get(id as usize).filter(|at| **at != Self::NONE) {
            return at as usize;
        }
        let words = self.words;
        let at = match seg.masks.exprs[id as usize] {
            Expr::Validity(slot) => match seg.slots[slot as usize].source {
                Source::Input(index) => match self.input.columns.get(index).and_then(|(_, c)| c.validity) {
                    Some((bits, offset)) => self.push(&Bits::window(bits, offset, self.rows)),
                    None => Self::ONES,
                },
                Source::Op => {
                    let bits = self.leaf(slot).validity(self.rows);
                    self.push(&bits)
                }
            },
            Expr::Valued(slot) => {
                let bits = self.leaf(slot).valued(self.rows);
                self.push(&bits)
            }
            Expr::And(a, b) | Expr::Or(a, b) => {
                let (a, b) = (self.region(seg, a), self.region(seg, b));
                let at = self.arena.len();
                self.arena.resize(at + words, 0);
                let and = matches!(seg.masks.exprs[id as usize], Expr::And(..));
                for w in 0..words {
                    self.arena[at + w] = match and {
                        true => self.arena[a + w] & self.arena[b + w],
                        false => self.arena[a + w] | self.arena[b + w],
                    };
                }
                at
            }
            Expr::Not(a) => {
                let a = self.region(seg, a);
                let at = self.arena.len();
                self.arena.resize(at + words, 0);
                for w in 0..words {
                    self.arena[at + w] = !self.arena[a + w] & self.arena[Self::ONES + w];
                }
                at
            }
            Expr::Produced(_) => self.words,
        };
        if let Some(slot) = self.offsets.get_mut(id as usize) {
            *slot = at as u32;
        }
        at
    }

    fn produce(&mut self, m: M, bits: Rc<[u64]>) {
        if let M::Id(id) = m {
            let at = self.push(&bits);
            if let Some(slot) = self.offsets.get_mut(id as usize) {
                *slot = at as u32;
            }
        }
    }

    fn implied(seg: &SegPlan, r: Ref) -> bool {
        match r.present {
            M::All => true,
            M::None => false,
            M::Id(id) => matches!(seg.masks.exprs[id as usize], Expr::Validity(s) | Expr::Valued(s) if s == r.slot),
        }
    }

    fn value(&mut self, seg: &SegPlan, r: Ref) -> Leaf<'a> {
        let leaf = self.leaf(r.slot);
        if Self::implied(seg, r) {
            return leaf;
        }
        if let Some((_, _, cached)) = self.masked.iter().find(|(s, m, _)| *s == r.slot && *m == r.present) {
            return cached.clone();
        }
        let mask = self.mask(seg, r.present);
        let masked = Leaf::masked(leaf, &mask);
        self.masked.push((r.slot, r.present, masked.clone()));
        masked
    }

    fn fail(&mut self, row: usize, node: &crate::model::DecisionNode, message: Rc<str>) {
        if let Some(slot @ None) = self.errors.get_mut(row) {
            self.failed += 1;
            *slot = Some(Box::new(EvaluationError::NodeError {
                node_id: node.id.clone(),
                source: Box::new(Failure(message)),
                trace: None,
            }));
        }
    }

    fn shape(&mut self, seg: &SegPlan, node: &Node) -> Shaped<'a> {
        let leaf = node.leaf.map(|r| (self.value(seg, r), self.mask(seg, r.present)));
        let obj = node.obj.map(|m| self.mask(seg, m));
        let fields = node
            .fields
            .iter()
            .map(|(key, child)| (Symbol::from(key.as_ref()), self.shape(seg, child)))
            .collect();
        Shaped { leaf, obj, fields }
    }

    fn objects(shape: &Shaped<'a>, rows: usize) -> Vec<Variable> {
        let mut columns: Vec<Vec<Variable>> = shape.fields.iter().map(|(_, child)| Self::objects(child, rows)).collect();
        let full = {
            let mut map = VariableMap::with_capacity(shape.fields.len());
            shape.fields.iter().for_each(|(key, _)| {
                map.insert(key.clone(), Variable::Null);
            });
            map.shape().cloned()
        };
        let leaf = shape.leaf.as_ref().map(|(leaf, present)| (leaf.column(), present));
        (0..rows)
            .map(|row| {
                if let Some((column, present)) = &leaf {
                    if Bits::get(present, row) {
                        return column.variable(row);
                    }
                }
                if !shape.obj.as_ref().is_some_and(|obj| Bits::get(obj, row)) {
                    return Variable::Null;
                }
                let complete = shape.fields.iter().all(|(_, child)| Self::exists(child, row));
                match (complete, &full) {
                    (true, Some(full)) => Variable::from_object(VariableMap::from_shape(
                        full.clone(),
                        columns.iter_mut().map(|column| std::mem::replace(&mut column[row], Variable::Null)),
                    )),
                    _ => {
                        let mut map = VariableMap::with_capacity(shape.fields.len());
                        for ((key, child), column) in shape.fields.iter().zip(columns.iter_mut()) {
                            if Self::exists(child, row) {
                                map.insert(key.clone(), std::mem::replace(&mut column[row], Variable::Null));
                            }
                        }
                        Variable::from_object(map)
                    }
                }
            })
            .collect()
    }

    fn exists(shape: &Shaped<'a>, row: usize) -> bool {
        shape.leaf.as_ref().is_some_and(|(_, p)| Bits::get(p, row)) || shape.obj.as_ref().is_some_and(|o| Bits::get(o, row))
    }

    fn merged(a: &Shaped<'a>, b: &Shaped<'a>, rr: bool, rb: bool, ra: bool, row: usize) -> Option<Variable> {
        if ra {
            return Self::exists(a, row).then(|| Self::object(a, row));
        }
        if rb {
            return Self::exists(b, row).then(|| Self::object(b, row));
        }
        if !rr {
            return None;
        }
        if !Self::exists(b, row) {
            return Self::exists(a, row).then(|| Self::object(a, row));
        }
        let patch = Self::object(b, row);
        match (&patch, Self::exists(a, row)) {
            (Variable::Null, _) => None,
            (Variable::Object(_), true) => {
                let mut doc = Self::object(a, row);
                Some(match doc {
                    Variable::Object(_) => doc.merge_clone(&patch),
                    _ => patch,
                })
            }
            _ => Some(patch),
        }
    }

    fn object(shape: &Shaped<'a>, row: usize) -> Variable {
        if let Some((leaf, present)) = &shape.leaf {
            if Bits::get(present, row) {
                return leaf.get(row);
            }
        }
        if !shape.obj.as_ref().is_some_and(|obj| Bits::get(obj, row)) {
            return Variable::Null;
        }
        let mut map = VariableMap::with_capacity(shape.fields.len());
        for (key, child) in &shape.fields {
            let present = child.leaf.as_ref().is_some_and(|(_, p)| Bits::get(p, row))
                || child.obj.as_ref().is_some_and(|o| Bits::get(o, row));
            if present {
                map.insert(key.clone(), Self::object(child, row));
            }
        }
        Variable::from_object(map)
    }

    fn spread(&self, local: &Rc<[u64]>, positions: Option<&Rc<[usize]>>) -> Rc<[u64]> {
        match positions {
            None => local.clone(),
            Some(positions) => {
                let mut bits = vec![0u64; self.rows.div_ceil(64)];
                for (w, &word) in local.iter().enumerate() {
                    let mut word = word;
                    while word != 0 {
                        let i = w * 64 + word.trailing_zeros() as usize;
                        word &= word - 1;
                        if let Some(&row) = positions.get(i) {
                            Bits::set(&mut bits, row, true);
                        }
                    }
                }
                bits.into()
            }
        }
    }

    fn place(&self, leaf: Leaf<'a>, positions: Option<&Rc<[usize]>>) -> Leaf<'a> {
        match positions {
            None => leaf,
            Some(positions) => Leaf::scattered(leaf, positions.clone(), self.rows),
        }
    }
}

struct Run<'r, 'a> {
    graph: &'r CompiledGraph,
    layout: &'r Layout,
    runner: &'r mut LaneRunner,
    store: Store<'a>,
}

impl<'a> Run<'_, 'a> {
    fn alive(&self, positions: Option<Rc<[usize]>>, seen: &mut usize) -> Option<Rc<[usize]>> {
        if self.store.failed == *seen {
            return positions;
        }
        *seen = self.store.failed;
        let rows = self.store.rows;
        let current: Vec<usize> = match &positions {
            Some(p) => p.to_vec(),
            None => (0..rows).collect(),
        };
        let live: Vec<usize> = current.iter().copied().filter(|&row| self.store.errors[row].is_none()).collect();
        match (live.len() + live.len() / 7 < current.len(), live.len() == rows) {
            (true, _) => Some(live.into()),
            (false, true) => None,
            (false, false) => positions,
        }
    }

    fn segment(&mut self, seg: &SegPlan, positions: Option<Rc<[usize]>>) -> Result<(), String> {
        self.store.enter(seg);
        let mut seen = 0usize;
        let mut live = self.alive(positions, &mut seen);
        let mut positions = live.as_ref();
        for op in &seg.ops {
            if self.store.failed != seen {
                live = self.alive(live, &mut seen);
                positions = live.as_ref();
            }
            match op {
                Op::Lane(lane) => {
                    self.lane(seg, lane, positions, true);
                }
                Op::Table(table) => self.table(seg, table, positions),
                Op::Select { a, b, take, out } => {
                    let (a, b) = (self.store.value(seg, *a), self.store.value(seg, *b));
                    let take = self.store.mask(seg, *take);
                    let (a, b) = (a.column(), b.column());
                    let rows = self.store.rows;
                    let array = Self::selected(&a, &b, &take, rows).unwrap_or_else(|| {
                        let mut builder = ColumnBuilder::with_capacity(rows);
                        for row in 0..rows {
                            match Bits::get(&take, row) {
                                true => builder.push_cell(&b, row),
                                false => builder.push_cell(&a, row),
                            }
                        }
                        builder.finish()
                    });
                    self.store.slots[*out as usize] = Some(Leaf::typed(array));
                }
                Op::Materialize { node, out } => {
                    let shape = self.store.shape(seg, node);
                    let values = Store::objects(&shape, self.store.rows);
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
                Op::MergeRows { a, b, parts, present, out } => {
                    let (sa, sb) = (self.store.shape(seg, a), self.store.shape(seg, b));
                    let [rr, rb, ra] = parts.map(|m| self.store.mask(seg, m));
                    let rows = self.store.rows;
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    let values: Vec<Variable> = (0..rows)
                        .map(|row| {
                            let value = Store::merged(&sa, &sb, Bits::get(&rr, row), Bits::get(&rb, row), Bits::get(&ra, row), row);
                            if value.is_some() {
                                Bits::set(&mut bits, row, true);
                            }
                            value.unwrap_or(Variable::Null)
                        })
                        .collect();
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                    self.store.produce(*present, bits.into());
                }
                Op::Null { out } => {
                    self.store.slots[*out as usize] = Some(Leaf::nulls(self.store.rows));
                }
                Op::Coalesce { from, value, out } => {
                    let leaf = self.store.value(seg, *from);
                    let array = Self::coalesced(&leaf.column(), value, self.store.rows);
                    self.store.slots[*out as usize] = Some(Leaf::typed(array));
                }
                Op::Extract { from, path, out } => {
                    let leaf = self.store.value(seg, *from);
                    let path = path.iter().map(|s| s.as_ref()).collect::<Vec<_>>().join(".");
                    let values: Vec<Variable> = (0..self.store.rows)
                        .map(|row| Data::lookup(&leaf.get(row), &path).unwrap_or(Variable::Null))
                        .collect();
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
            }
        }
        match &seg.end {
            SegEnd::Finish(refs) => {
                let leaves = refs
                    .iter()
                    .map(|(path, r)| {
                        let base = self.store.leaf(r.slot);
                        let presence = (!Store::implied(seg, *r)).then(|| self.store.mask(seg, r.present));
                        (path.clone(), base, presence)
                    })
                    .collect();
                self.store.groups.push((positions.cloned(), leaves));
                Ok(())
            }
            SegEnd::Switch { first, ids, conditions, .. } => {
                let rows = self.store.rows;
                let truths: Vec<Option<Rc<[u64]>>> = conditions
                    .iter()
                    .map(|condition| match condition {
                        Cond::Always => Some(self.store.ones.clone()),
                        Cond::Never => None,
                        Cond::Lane(op) => {
                            let uniform = op.uniform;
                            let first: Rc<[usize]> = Rc::from([positions.and_then(|p| p.first().copied()).unwrap_or(0)]);
                            let local = match uniform {
                                true => Some(&first),
                                false => positions,
                            };
                            self.lane(seg, op, local, false);
                            let slot = op.outs.first().copied().unwrap_or_default();
                            let leaf = self.store.leaf(slot);
                            match (uniform, leaf.scatter_parts()) {
                                (true, Some((inner, _))) => inner.truthy(0).then(|| self.store.ones.clone()),
                                _ => Some(leaf.truths(rows).into()),
                            }
                        }
                    })
                    .collect();
                let words = conditions.len().div_ceil(64).max(1);
                let reach_bits: Vec<u64> = match positions {
                    None => self.store.ones.to_vec(),
                    Some(p) => {
                        let mut bits = vec![0u64; rows.div_ceil(64)];
                        p.iter().for_each(|&row| Bits::set(&mut bits, row, true));
                        bits
                    }
                };
                let uniform = truths.iter().all(|t| t.as_ref().is_none_or(|t| Rc::ptr_eq(t, &self.store.ones)));
                let mut groups: Vec<(Vec<u64>, Vec<usize>)> = Vec::new();
                let members = |bits: &[u64]| -> Vec<usize> {
                    bits.iter()
                        .enumerate()
                        .flat_map(|(w, &word)| {
                            let mut word = word;
                            std::iter::from_fn(move || {
                                (word != 0).then(|| {
                                    let bit = word.trailing_zeros() as usize;
                                    word &= word - 1;
                                    w * 64 + bit
                                })
                            })
                        })
                        .collect()
                };
                if uniform || *first {
                    let mut remaining = reach_bits.clone();
                    for (index, truth) in truths.iter().enumerate() {
                        let Some(truth) = truth else {
                            continue;
                        };
                        let hit: Vec<u64> = remaining.iter().zip(truth.iter()).map(|(r, t)| r & t).collect();
                        if Bits::none(&hit) {
                            continue;
                        }
                        let mut key = vec![0u64; words];
                        if uniform && !*first {
                            for (i, t) in truths.iter().enumerate() {
                                if t.is_some() {
                                    key[i / 64] |= 1 << (i % 64);
                                }
                            }
                            groups.push((key, members(&hit)));
                            remaining.iter_mut().for_each(|r| *r = 0);
                            break;
                        }
                        key[index / 64] |= 1 << (index % 64);
                        remaining.iter_mut().zip(&hit).for_each(|(r, h)| *r &= !h);
                        groups.push((key, match hit == reach_bits {
                            true => Vec::new(),
                            false => members(&hit),
                        }));
                    }
                    if !Bits::none(&remaining) {
                        groups.push((vec![0u64; words], match remaining == reach_bits {
                            true => Vec::new(),
                            false => members(&remaining),
                        }));
                    }
                } else if words == 1 {
                    let mut keyed: Vec<(u64, Vec<usize>)> = Vec::new();
                    let mut last = 0usize;
                    let mut visit = |row: usize| {
                        let key = truths
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| t.as_ref().is_some_and(|t| Bits::get(t, row)))
                            .fold(0u64, |k, (index, _)| k | 1 << index);
                        let at = match keyed.get(last).is_some_and(|(k, _)| *k == key) {
                            true => last,
                            false => match keyed.iter().position(|(k, _)| *k == key) {
                                Some(g) => g,
                                None => {
                                    keyed.push((key, Vec::new()));
                                    keyed.len() - 1
                                }
                            },
                        };
                        keyed[at].1.push(row);
                        last = at;
                    };
                    match positions {
                        Some(p) => p.iter().for_each(|&row| visit(row)),
                        None => (0..rows).for_each(&mut visit),
                    }
                    groups.extend(keyed.into_iter().map(|(key, members)| (vec![key], members)));
                } else {
                    let mut key = vec![0u64; words];
                    let mut last: Option<usize> = None;
                    let reach: Vec<usize> = match positions {
                        Some(p) => p.to_vec(),
                        None => (0..rows).collect(),
                    };
                    for &row in &reach {
                        key.iter_mut().for_each(|w| *w = 0);
                        for (index, truth) in truths.iter().enumerate() {
                            if truth.as_ref().is_some_and(|t| Bits::get(t, row)) {
                                key[index / 64] |= 1 << (index % 64);
                            }
                        }
                        let at = match last.filter(|&g| groups[g].0 == key) {
                            Some(g) => g,
                            None => match groups.iter().position(|(k, _)| *k == key) {
                                Some(g) => g,
                                None => {
                                    groups.push((key.clone(), Vec::new()));
                                    groups.len() - 1
                                }
                            },
                        };
                        groups[at].1.push(row);
                        last = Some(at);
                    }
                }
                for (key, members) in groups {
                    let child = seg.child(self.graph, self.layout, &key);
                    let child = child.as_ref().as_ref().map_err(String::clone)?;
                    self.store.rewind(seg);
                    let members: Option<Rc<[usize]>> = match (members.is_empty(), members.len() == rows) {
                        (true, _) => positions.cloned(),
                        (false, true) => None,
                        (false, false) => Some(members.into()),
                    };
                    self.segment(child, members)?;
                }
                Ok(())
            }
        }
    }

    fn program<'g>(graph: &'g CompiledGraph, op: &'g LaneOp) -> Option<(&'g LaneProgram, Entries<'g>)> {
        if let Some(own) = &op.own {
            return Some((&own.program, Some(&own.entries)));
        }
        let step = graph.step_at(op.node)?;
        match (&step.kind, op.program) {
            (Kind::Node { body: Body::Expression { program, entries, .. }, .. }, Program::Expression) => Some((program, Some(entries))),
            (Kind::Switch { conditions, .. }, Program::Condition(index)) => match conditions.get(index) {
                Some((_, Condition::Program(program))) => Some((program, None)),
                _ => None,
            },
            _ => None,
        }
    }

    fn distinct(leaves: &[Leaf<'a>], hints: &[Option<(Rc<[i32]>, usize)>], positions: Option<&Rc<[usize]>>, rows: usize) -> Option<(Vec<usize>, Vec<u32>)> {
        let local = positions.map_or(rows, |p| p.len());
        if leaves.is_empty() || local < 16 {
            return None;
        }
        let columns: Vec<Column> = leaves.iter().map(Leaf::column).collect();
        let mut strides = Vec::with_capacity(columns.len());
        let mut size = 1u64;
        for (at, column) in columns.iter().enumerate() {
            let width = match (column.values, hints.get(at).and_then(Option::as_ref)) {
                (_, Some((_, size))) => *size as u64 + 1,
                (Values::Dict { values, .. }, None) => values.len() as u64 + 1,
                (Values::Bool { .. }, None) => 3,
                _ => return None,
            };
            strides.push(size);
            size = size.checked_mul(width)?;
        }
        let mut keys = vec![0u64; local];
        for (index, (column, stride)) in columns.iter().zip(&strides).enumerate() {
            let hint = hints.get(index).and_then(Option::as_ref);
            let code = |row: usize, code: i32| match column.valid(row) {
                true => u64::try_from(code).map_or(0, |k| k + 1),
                false => 0,
            };
            let codes: Option<&[i32]> = match (hint, column.values) {
                (Some((codes, _)), _) => Some(codes),
                (None, Values::Dict { keys, .. }) => Some(keys),
                _ => None,
            };
            match (codes, column.values) {
                (Some(codes), _) if positions.is_none() && column.validity.is_none() && codes.len() >= local => {
                    for (key, &code) in keys.iter_mut().zip(codes) {
                        *key += (code.max(-1) + 1) as u64 * stride;
                    }
                }
                (Some(codes), _) if positions.is_none() && codes.len() >= local => {
                    let valid = column.validity.map_or_else(|| Bits::ones(local), |(bits, offset)| Bits::window(bits, offset, local));
                    for ((at, key), &code) in keys.iter_mut().enumerate().zip(codes) {
                        let live = (valid[at >> 6] >> (at & 63)) & 1;
                        *key += (code.max(-1) + 1) as u64 * live * stride;
                    }
                }
                (Some(codes), _) => {
                    for (at, key) in keys.iter_mut().enumerate() {
                        let row = positions.map_or(at, |p| p[at]);
                        *key += code(row, codes.get(row).copied().unwrap_or(-1)) * stride;
                    }
                }
                (None, Values::Bool { bits, offset }) => {
                    for (at, key) in keys.iter_mut().enumerate() {
                        let row = positions.map_or(at, |p| p[at]);
                        *key += code(row, i32::from(Bits::get(bits, offset + row))) * stride;
                    }
                }
                _ => return None,
            }
        }
        let dense = size <= 4096;
        let slots = match dense {
            true => size as usize,
            false => (local / 2).next_power_of_two().max(16) * 2,
        };
        let mut table = vec![(u64::MAX, u32::MAX); slots];
        let mask = slots - 1;
        let mut reps = Vec::new();
        let mut ids = Vec::with_capacity(local);
        for (at, &key) in keys.iter().enumerate() {
            let mut slot = match dense {
                true => key as usize,
                false => (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & mask,
            };
            while !dense && table[slot].1 != u32::MAX && table[slot].0 != key {
                slot = (slot + 1) & mask;
            }
            let entry = table.get_mut(slot)?;
            if entry.1 == u32::MAX {
                *entry = (key, reps.len() as u32);
                reps.push(positions.map_or(at, |p| p[at]));
                if reps.len() * 4 > local {
                    return None;
                }
            }
            ids.push(entry.1);
        }
        Some((reps, ids))
    }

    fn lane(&mut self, seg: &SegPlan, op: &LaneOp, positions: Option<&Rc<[usize]>>, report: bool) {
        let Some((program, entries)) = Self::program(self.graph, op) else {
            return;
        };
        let rows = self.store.rows;
        let leaves: Vec<Leaf<'a>> = op.reads.iter().map(|(_, r)| self.store.value(seg, *r)).collect();
        let mut columns = Columns::new(rows);
        columns.columns.reserve_exact(op.reads.len());
        for ((key, _), leaf) in op.reads.iter().zip(&leaves) {
            columns = columns.column(key.as_ref(), leaf.column());
        }
        let program = op
            .specialized
            .get_or_init(|| program.specialize_columns(&columns).ok())
            .as_ref()
            .unwrap_or(program);
        let scopes = Bound::blank(rows);
        let mut outs = Outs::take();
        let mut failures: Vec<(usize, usize)> = Vec::new();
        let local = positions.map_or(rows, |p| p.len());
        let single = [positions.and_then(|p| p.first().copied()).unwrap_or(0)];
        let uniform = op.uniform && local > 1;
        let keyed = match op.stable && !uniform {
            true => {
                let hints: Vec<Option<(Rc<[i32]>, usize)>> = op
                    .reads
                    .iter()
                    .map(|(_, r)| self.store.hints.iter().find(|(slot, _, _)| *slot == r.slot).map(|(_, codes, size)| (codes.clone(), *size)))
                    .collect();
                Self::distinct(&leaves, &hints, positions, rows)
            }
            false => None,
        };
        let subset = match (uniform, &keyed) {
            (true, _) => Some(&single[..]),
            (false, Some((reps, _))) => Some(&reps[..]),
            (false, None) => positions.map(|p| p.as_ref()),
        };
        self.runner.evaluate_sites(program, scopes.slice(), &columns, &op.bound, subset, &mut outs, |row, stage, _| {
            failures.push((row, stage))
        });
        if uniform {
            let stages: Vec<usize> = failures.iter().map(|(_, stage)| *stage).collect();
            failures = stages.into_iter().flat_map(|stage| (0..local).map(move |at| (at, stage))).collect();
        }
        if let (Some((reps, ids)), false) = (&keyed, failures.is_empty()) {
            let mut stages: Vec<Vec<usize>> = vec![Vec::new(); reps.len()];
            failures.iter().for_each(|&(rep, stage)| stages[rep].push(stage));
            failures = ids
                .iter()
                .enumerate()
                .flat_map(|(at, id)| stages[*id as usize].iter().map(move |&stage| (at, stage)))
                .collect();
        }
        for (slot, out) in op.outs.iter().zip(outs.iter_mut()) {
            let array = Array::from_output(out);
            let leaf = match (uniform, &keyed) {
                (true, _) => array.broadcast(local).map_or_else(|| Leaf::nulls(local), Leaf::typed),
                (false, Some((_, ids))) => Leaf::typed(array.expand(ids)),
                (false, None) => Leaf::typed(array),
            };
            let leaf = self.store.place(leaf, positions);
            self.store.slots[*slot as usize] = Some(leaf);
        }
        let (Some(entries), true, Some(step)) = (entries, report, self.graph.step_at(op.node)) else {
            return;
        };
        let mut messages: Vec<Option<Rc<str>>> = vec![None; entries.len().max(1)];
        for (at, stage) in failures {
            let row = positions.map_or(at, |p| p.get(at).copied().unwrap_or(at));
            if self.store.errors.get(row).is_none_or(Option::is_some) {
                continue;
            }
            let message = messages.get_mut(stage).map(|m| {
                m.get_or_insert_with(|| {
                    let source = entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default();
                    Rc::from(format!(r#"Failed to evaluate expression: "{source}""#))
                })
                .clone()
            });
            self.store.fail(row, &step.node, message.unwrap_or_else(|| Rc::from("")));
        }
    }

    fn table(&mut self, seg: &SegPlan, op: &TableOp, positions: Option<&Rc<[usize]>>) {
        let Some(step) = self.graph.step_at(op.node) else {
            return;
        };
        let Kind::Node {
            body: Body::Table(table),
            ..
        } = &step.kind
        else {
            return;
        };
        let local = positions.map_or(self.store.rows, |p| p.len());
        let mut leaves: Vec<Option<Leaf<'a>>> = op
            .reads
            .iter()
            .map(|(_, r)| {
                r.map(|r| {
                    let leaf = self.store.value(seg, r);
                    match positions {
                        Some(positions) => leaf.pick(positions),
                        None => leaf,
                    }
                })
            })
            .collect();
        let keyed = match (op.mode, table.stable) {
            (TableMode::First, true) => {
                let present: Vec<Leaf<'a>> = leaves.iter().flatten().cloned().collect();
                match present.len() > 1 {
                    true => Self::distinct(&present, &[], None, local),
                    false => None,
                }
            }
            _ => None,
        };
        let evaluated = match &keyed {
            Some((reps, _)) => {
                let reps: Rc<[usize]> = reps.as_slice().into();
                leaves = leaves.into_iter().map(|leaf| leaf.map(|leaf| leaf.pick(&reps))).collect();
                reps.len()
            }
            None => local,
        };
        let mut columns = Columns::new(evaluated);
        columns.columns.reserve_exact(op.reads.len());
        let mut names: Vec<(Arc<str>, Bind)> = Vec::with_capacity(op.reads.len());
        let views: Vec<Option<Column>> = leaves.iter().map(|leaf| leaf.as_ref().map(Leaf::column)).collect();
        for ((key, _), view) in op.reads.iter().zip(&views) {
            match view {
                Some(view) => {
                    names.push((key.clone(), Bind::Column(columns.columns.len())));
                    columns = columns.column(key.as_ref(), *view);
                }
                None => names.push((key.clone(), Bind::Absent)),
            }
        }
        let bound = Bound {
            scopes: Bound::blank(evaluated),
            columns,
            bind: Box::new(move |key: &str| match key.starts_with('$') {
                true => Bind::Row,
                false => names
                    .iter()
                    .find(|(k, _)| k.as_ref() == key)
                    .map_or(Bind::Absent, |(_, b)| *b),
            }),
        };
        let (columns, present, unmatched) = match op.mode {
            TableMode::First => table.columns(table.first(self.runner, &bound)),
            TableMode::Collected => {
                let collected = table.first_collect(self.runner, &bound);
                (collected.columns, collected.present, collected.unmatched)
            }
            TableMode::Rows => {
                let (values, hint) = match table.collect_keyed(self.runner, &bound) {
                    Some((values, codes, size)) => {
                        let codes: Rc<[i32]> = match positions {
                            None => codes.into(),
                            Some(positions) => {
                                let mut full = vec![-1i32; self.store.rows];
                                positions.iter().zip(&codes).for_each(|(&row, &code)| full[row] = code);
                                full.into()
                            }
                        };
                        (values, Some((codes, size)))
                    }
                    None => (
                        table.evaluate(self.runner, &bound).into_iter().map(|out| table.value(out)).collect::<Vec<Variable>>(),
                        None,
                    ),
                };
                let leaf = self.store.place(Leaf::Any(values.into()), positions);
                if let Some(slot) = op.outs.first() {
                    self.store.slots[*slot as usize] = Some(leaf);
                    if let Some((codes, size)) = hint {
                        self.store.hints.push((*slot, codes, size));
                    }
                }
                return;
            }
        };
        let (columns, present, unmatched) = match &keyed {
            Some((_, ids)) => (
                columns.iter().map(|leaf| leaf.expand(ids)).collect(),
                present
                    .iter()
                    .map(|mask| Mask::Bits(Bits::of(local, |row| mask.get(ids[row] as usize)).into()))
                    .collect(),
                Bits::of(local, |row| Bits::get(&unmatched, ids[row] as usize)),
            ),
            None => (columns, present, unmatched),
        };
        let mut spread: Vec<Spread> = Vec::new();
        let mut cached = |store: &Store<'a>, dense: Rc<[u64]>| match spread.iter().find(|(d, _)| Rc::ptr_eq(d, &dense) || *d == dense) {
            Some((_, bits)) => bits.clone(),
            None => {
                let bits = store.spread(&dense, positions);
                spread.push((dense, bits.clone()));
                bits
            }
        };
        for ((slot, leaf), (m, mask)) in op.outs.iter().zip(columns).zip(op.present.iter().zip(present)) {
            let leaf = self.store.place(leaf, positions);
            self.store.slots[*slot as usize] = Some(leaf);
            let bits = cached(&self.store, mask.dense(local));
            self.store.produce(*m, bits);
        }
        let mut matched: Vec<u64> = unmatched.iter().map(|w| !w).collect();
        Bits::trim(&mut matched, local);
        let matched = cached(&self.store, matched.into());
        self.store.produce(op.matched, matched);
    }

    fn coded_gather<'v>(rows: usize, views: &'v [Option<View<'v>>], located: &dyn Fn(usize) -> Option<(usize, &'v Column<'v>, usize)>) -> Option<Array> {
        let mut unique: Vec<&Rc<Dict>> = Vec::new();
        let mut slot = Vec::with_capacity(views.len());
        for view in views {
            match view {
                None => slot.push(0),
                Some((_, _, _, dict)) => {
                    let dict = (*dict)?;
                    let index = match unique.iter().position(|d| Rc::ptr_eq(d, dict)) {
                        Some(index) => index,
                        None => {
                            unique.push(dict);
                            unique.len() - 1
                        }
                    };
                    slot.push(index);
                }
            }
        }
        let (dict, bases) = Dict::concat(&unique)?;
        let mut valid = vec![0u64; rows.div_ceil(64)];
        let codes: Rc<[i32]> = (0..rows)
            .map(|row| {
                let Some((g, view, at)) = located(row) else {
                    return -1;
                };
                match view.code(at) {
                    Some(code) => {
                        valid[row >> 6] |= 1 << (row & 63);
                        code as i32 + bases[slot[g]]
                    }
                    None => -1,
                }
            })
            .collect();
        Some(Array::coded_with(codes, dict, valid))
    }

    fn coalesced(column: &Column, value: &Literal, rows: usize) -> Array {
        let typed = match value {
            Literal::Num(n) => Literal::Num(*n).scaled().and_then(|fill| {
                let (mut mant, mut scale) = (vec![0i64; rows], vec![0u8; rows]);
                for row in 0..rows {
                    let (m, s) = match column.valid(row) {
                        false => fill,
                        true => match column.values {
                            Values::Scaled { mant, scale } => mant.get(row).zip(scale.get(row)).map(|(m, s)| (*m, *s))?,
                            Values::I64(values) => (*values.get(row)?, 0),
                            Values::Dec(values) => {
                                let d = values.get(row)?;
                                if d.is_zero() && d.is_sign_negative() {
                                    return None;
                                }
                                (i64::try_from(d.mantissa()).ok()?, u8::try_from(d.scale()).ok()?)
                            }
                            _ => return None,
                        },
                    };
                    mant[row] = m;
                    scale[row] = s;
                }
                Some(Array::parts(Buffer::Scaled { mant, scale }, None, rows))
            }),
            Literal::Str(fill) => {
                let mut offsets = Vec::with_capacity(rows + 1);
                let mut data = String::new();
                offsets.push(0i32);
                let mut ok = true;
                for row in 0..rows {
                    match column.valid(row) {
                        false => data.push_str(fill),
                        true => match column.text(row) {
                            Some(text) => data.push_str(text),
                            None => {
                                ok = false;
                                break;
                            }
                        },
                    }
                    offsets.push(data.len() as i32);
                }
                ok.then(|| Array::parts(Buffer::Text { offsets, data }, None, rows))
            }
            Literal::Bool(fill) => {
                let mut ok = true;
                let bits = Bits::of(rows, |row| match column.valid(row) {
                    false => *fill,
                    true => column.boolean(row).unwrap_or_else(|| {
                        ok = false;
                        false
                    }),
                });
                ok.then(|| Array::parts(Buffer::Bool(bits), None, rows))
            }
            _ => None,
        };
        typed.unwrap_or_else(|| {
            let mut builder = ColumnBuilder::with_capacity(rows);
            for row in 0..rows {
                match column.valid(row) {
                    true => builder.push_cell(column, row),
                    false => builder.push_literal(value),
                }
            }
            builder.finish()
        })
    }

    fn selected(a: &Column, b: &Column, take: &[u64], rows: usize) -> Option<Array> {
        let scaled = |column: &Column, row: usize| -> Option<(i64, u8)> {
            match column.values {
                Values::Scaled { mant, scale } => mant.get(row).zip(scale.get(row)).map(|(m, s)| (*m, *s)),
                Values::I64(values) => values.get(row).map(|v| (*v, 0)),
                Values::Dict {
                    keys,
                    values: Dictionary::Scaled { mant, scale },
                } => keys
                    .get(row)
                    .and_then(|k| usize::try_from(*k).ok())
                    .and_then(|c| mant.get(c).zip(scale.get(c)))
                    .map(|(m, s)| (*m, *s)),
                _ => None,
            }
        };
        let numeric = |column: &Column| {
            matches!(column.values, Values::Scaled { .. } | Values::I64(_) | Values::Dict { values: Dictionary::Scaled { .. }, .. })
                || (0..rows).all(|row| !column.valid(row))
        };
        if !numeric(a) || !numeric(b) {
            return None;
        }
        let (mut mant, mut scale) = (vec![0i64; rows], vec![0u8; rows]);
        let mut valid = vec![0u64; rows.div_ceil(64)];
        for row in 0..rows {
            let source = match Bits::get(take, row) {
                true => b,
                false => a,
            };
            if !source.valid(row) {
                continue;
            }
            let (m, s) = scaled(source, row)?;
            mant[row] = m;
            scale[row] = s;
            valid[row >> 6] |= 1 << (row & 63);
        }
        Some(Array::parts(Buffer::Scaled { mant, scale }, Some(valid), rows))
    }

    fn typed_gather<'c>(rows: usize, family: Option<u8>, cell: &dyn Fn(usize) -> Option<(&'c Column<'c>, usize)>) -> Option<Array> {
        let mut valid = vec![0u64; rows.div_ceil(64)];
        match family? {
            0 => {
                let (mut mant, mut scale) = (vec![0i64; rows], vec![0u8; rows]);
                for row in 0..rows {
                    let Some((view, at)) = cell(row) else {
                        continue;
                    };
                    let parts = match view.values {
                        Values::Scaled { mant, scale } => mant.get(at).zip(scale.get(at)).map(|(m, s)| (*m, *s)),
                        Values::I64(values) => values.get(at).map(|v| (*v, 0)),
                        Values::Dict {
                            keys,
                            values: Dictionary::Scaled { mant, scale },
                        } => keys
                            .get(at)
                            .and_then(|k| usize::try_from(*k).ok())
                            .and_then(|c| mant.get(c).zip(scale.get(c)))
                            .map(|(m, s)| (*m, *s)),
                        _ => None,
                    };
                    if let Some((m, s)) = parts {
                        mant[row] = m;
                        scale[row] = s;
                        valid[row >> 6] |= 1 << (row & 63);
                    }
                }
                Some(Array::parts(Buffer::Scaled { mant, scale }, Some(valid), rows))
            }
            1 => {
                let mut bits = vec![0u64; rows.div_ceil(64)];
                for row in 0..rows {
                    if let Some(b) = cell(row).and_then(|(view, at)| view.boolean(at)) {
                        Bits::set(&mut bits, row, b);
                        valid[row >> 6] |= 1 << (row & 63);
                    }
                }
                Some(Array::parts(Buffer::Bool(bits), Some(valid), rows))
            }
            2 => {
                let mut offsets = Vec::with_capacity(rows + 1);
                let mut data = String::new();
                offsets.push(0i32);
                for row in 0..rows {
                    if let Some(text) = cell(row).and_then(|(view, at)| view.text(at)) {
                        data.push_str(text);
                        valid[row >> 6] |= 1 << (row & 63);
                    }
                    offsets.push(data.len() as i32);
                }
                Some(Array::parts(Buffer::Text { offsets, data }, Some(valid), rows))
            }
            _ => None,
        }
    }

    fn assemble(store: Store<'a>) -> ColumnarOutput<'a> {
        let rows = store.rows;
        let mut store = store;
        let groups = std::mem::take(&mut store.groups);
        let finish = |leaf: &Leaf<'a>, presence: &Option<Rc<[u64]>>| match presence {
            None => leaf.clone(),
            Some(mask) => Leaf::masked(leaf.clone(), mask),
        };
        let columns: Vec<(Arc<str>, Leaf<'a>)> = match groups.as_slice() {
            [(None, leaves)] => leaves.iter().map(|(path, leaf, presence)| (path.clone(), finish(leaf, presence))).collect(),
            _ => {
                let mut owner: Vec<Option<(usize, usize)>> = vec![None; rows];
                let mut reach: Vec<Vec<u64>> = Vec::with_capacity(groups.len());
                for (g, (positions, _)) in groups.iter().enumerate() {
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    match positions {
                        Some(positions) => positions.iter().enumerate().for_each(|(i, &row)| {
                            owner[row] = Some((g, i));
                            Bits::set(&mut bits, row, true);
                        }),
                        None => (0..rows).for_each(|row| {
                            owner[row] = Some((g, row));
                            Bits::set(&mut bits, row, true);
                        }),
                    }
                    reach.push(bits);
                }
                let mut paths: Vec<Arc<str>> = Vec::new();
                for (_, leaves) in &groups {
                    for (path, _, _) in leaves {
                        if !paths.contains(path) {
                            paths.push(path.clone());
                        }
                    }
                }
                paths
                    .into_iter()
                    .map(|path| {
                        let sources: Vec<Origin<'_, 'a>> = groups
                            .iter()
                            .map(|(_, leaves)| leaves.iter().find(|(p, _, _)| *p == path).map(|(_, l, m)| (l, m)))
                            .collect();
                        if let Some(Some((first, _))) = sources.iter().find(|s| s.is_some()) {
                            if sources.iter().flatten().all(|(s, _)| s.same(first)) {
                                if sources.iter().all(|s| s.is_some_and(|(_, m)| m.is_none())) {
                                    return (path, (*first).clone());
                                }
                                let mut combined = vec![0u64; rows.div_ceil(64)];
                                for (source, bits) in sources.iter().zip(&reach) {
                                    match source {
                                        None => {}
                                        Some((_, Some(mask))) => combined.iter_mut().zip(bits.iter().zip(mask.iter())).for_each(|(c, (b, m))| *c |= b & m),
                                        Some((_, None)) => combined.iter_mut().zip(bits).for_each(|(c, b)| *c |= b),
                                    }
                                }
                                return (path, Leaf::masked((*first).clone(), &combined.into()));
                            }
                        }
                        let views: Vec<Option<View>> = sources
                            .iter()
                            .zip(&groups)
                            .map(|(source, (positions, _))| {
                                source.map(|(leaf, presence)| match (leaf.scatter_parts(), positions) {
                                    (Some((inner, at)), Some(positions)) if Rc::ptr_eq(at, positions) => {
                                        (inner.column(), true, presence.as_ref(), inner.coded_dict())
                                    }
                                    _ => (leaf.column(), false, presence.as_ref(), leaf.coded_dict()),
                                })
                            })
                            .collect();
                        let located = |row: usize| -> Option<(usize, &Column, usize)> {
                            let (g, i) = owner[row]?;
                            let (view, local, presence, _) = views[g].as_ref()?;
                            if presence.is_some_and(|p| !Bits::get(p, row)) {
                                return None;
                            }
                            let at = if *local { i } else { row };
                            view.valid(at).then_some((g, view, at))
                        };
                        let cell = |row: usize| located(row).map(|(_, view, at)| (view, at));
                        if let Some(array) = Self::coded_gather(rows, &views, &located) {
                            return (path, Leaf::typed(array));
                        }
                        let family = |column: &Column| match column.values {
                            Values::Scaled { .. } | Values::I64(_) | Values::Dict { values: Dictionary::Scaled { .. }, .. } => 0u8,
                            Values::Bool { .. } | Values::Dict { values: Dictionary::Bool { .. }, .. } => 1,
                            Values::Text { .. } | Values::Utf8 { .. } | Values::Dict { values: Dictionary::Text { .. }, .. } => 2,
                            _ => 3,
                        };
                        let families: Vec<u8> = views.iter().flatten().map(|(v, _, _, _)| family(v)).collect();
                        let common = families.first().copied().filter(|f| families.iter().all(|x| x == f));
                        if let Some(array) = Self::typed_gather(rows, common, &cell) {
                            return (path, Leaf::typed(array));
                        }
                        let mut builder = ColumnBuilder::with_capacity(rows);
                        for row in 0..rows {
                            match cell(row) {
                                Some((view, at)) => builder.push_cell(view, at),
                                None => builder.push_null(),
                            }
                        }
                        (path, Leaf::typed(builder.finish()))
                    })
                    .collect()
            }
        };
        ColumnarOutput {
            rows,
            columns: columns
                .into_iter()
                .map(|(path, leaf)| (path, OutputColumn(CompiledGraph::cleaned(&leaf))))
                .collect(),
            errors: std::mem::take(&mut store.errors),
        }
    }
}

impl Plan {
    pub fn evaluate<'a>(
        &self,
        graph: &CompiledGraph,
        content: &GraphContent,
        columns: &'a Columns<'a>,
        failures: Vec<(usize, Box<EvaluationError>)>,
    ) -> Option<ColumnarOutput<'a>> {
        if let Some(resolved) = &content.resolved_schemas {
            let touched = graph
                .steps
                .iter()
                .flatten()
                .any(|step| !matches!(step.kind, Kind::Input) && resolved.contains_key(&step.node.id));
            if touched {
                return None;
            }
        }
        let (layout, root) = self.root(graph, columns)?;
        let root = root.as_ref().as_ref().ok()?;
        let mut store = Store::new(root, columns);
        for (row, error) in failures {
            if let Some(slot) = store.errors.get_mut(row) {
                if slot.is_none() {
                    store.failed += 1;
                }
                *slot = Some(error);
            }
        }
        CompiledGraph::RUNNER.with_borrow_mut(|runner| {
            let mut run = Run {
                graph,
                layout: &layout,
                runner,
                store,
            };
            run.segment(root, None).ok()?;
            Some(Run::assemble(run.store))
        })
    }
}

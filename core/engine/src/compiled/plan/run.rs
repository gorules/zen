use super::view::{Expr, MaskId, M, Node, Ref, SlotId};
use crate::compiled::table::{First, Table};
use crate::compiled::data::{Data, Layer, Mask};
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;
use super::typed::{Deferred, Grouping, Path, Piece, Target, TypedOp};
use super::{Cond, LaneOp, Layout, Op, Plan, Program, RowsCond, RowsOp, SegEnd, SegPlan, Source, TableMode, TableOp};
use zen_expression::typed::{Batch, Data as Slotted, Outputs, Slot};
use crate::decision_graph::cleaner::VariableCleaner;
use crate::decision_graph::walker::GraphWalker;
use crate::nodes::{NodeError, NodeHandlerExtensions};
use crate::compiled::bound::{Bound, Outs};
use crate::compiled::shred::{Listing, Normalizer, Seen, Shred, Shredder};
use crate::compiled::typed::{Array, Bits, ColumnBuilder, Dict, Field, Items, Leaf, Literal, Records, Store as Buffer, Templated};
use crate::compiled::data::Shape as DataShape;
use crate::compiled::{Body, ColumnarOutput, CompiledGraph, Condition, Kind, OutputColumn};
use crate::model::{DecisionNodeKind, GraphContent};
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use std::sync::Arc;
use zen_expression::lane::{Binding as Bind, Column, Columns, Dictionary, LaneProgram, LaneRunner, Values};

struct Failure(Rc<str>);

type Shredded<'a> = Option<(Rc<Records<'a>>, Rc<[u64]>)>;

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
    same: Vec<Option<usize>>,
}

type Entries<'g> = Option<&'g [(Arc<str>, Arc<str>)]>;
type Spread = (Rc<[u64]>, Rc<[u64]>);
type View<'v> = (Column<'v>, bool, Option<&'v Rc<[u64]>>, Option<&'v Rc<Dict>>);
type Origin<'s, 'a> = Option<(&'s Leaf<'a>, &'s Option<Rc<[u64]>>)>;

type Group<'a> = (Option<Rc<[usize]>>, Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)>);
type Lowered = (Outputs, Option<Vec<u32>>, Option<Vec<u64>>, Option<Vec<u64>>);
type Shown<'a> = (Leaf<'a>, Option<Rc<[u64]>>);
type Cached<'a> = (SlotId, Rc<str>, Option<Rc<Listing<'a>>>);
type Prepared<'a> = (Rc<Listing<'a>>, Option<Vec<u64>>, Option<Vec<u64>>);

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
    errors: Vec<Option<Rc<EvaluationError>>>,
    recent: Vec<(Arc<str>, Rc<str>, Rc<EvaluationError>)>,
    failed: usize,
    groups: Vec<Group<'a>>,
    hints: Vec<(SlotId, Rc<[i32]>, usize)>,
    views: Vec<(SlotId, bool, Option<Shown<'a>>)>,
    viewed: Vec<(SlotId, M, bool, Leaf<'a>)>,
    listings: std::cell::RefCell<Vec<Cached<'a>>>,
    counts: (usize, usize),
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
    const RECENT: usize = 8;

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
            recent: Vec::new(),
            failed: 0,
            groups: Vec::new(),
            hints: Vec::new(),
            views: Vec::new(),
            viewed: Vec::new(),
            listings: std::cell::RefCell::new(Vec::new()),
            counts: (0, 0),
        }
    }

    fn enter(&mut self, seg: &SegPlan) {
        self.slots.resize(seg.slots.len(), None);
        self.offsets.resize(seg.masks.exprs.len(), Self::NONE);
    }

    fn rewind(&mut self, (slots, masks): (usize, usize)) {
        self.slots.truncate(slots);
        self.offsets.truncate(masks);
        let kept = |slot: SlotId, m: M| {
            (slot as usize) < slots
                && match m {
                    M::Id(id) => (id as usize) < masks,
                    _ => true,
                }
        };
        self.masked.retain(|(slot, m, _)| kept(*slot, *m));
        self.views.retain(|(slot, ..)| kept(*slot, M::All));
        self.viewed.retain(|(slot, m, ..)| kept(*slot, *m));
        self.listings.get_mut().retain(|(slot, ..)| kept(*slot, M::All));
    }

    fn listing(&self, slot: SlotId, chain: &[&str]) -> Option<Rc<Listing<'a>>> {
        let key = chain.join("\n");
        if let Some((.., found)) = self.listings.borrow().iter().find(|(s, k, _)| *s == slot && k.as_ref() == key) {
            return found.clone();
        }
        let built = match chain.split_last() {
            None => Listing::leaf(&self.leaf(slot), self.rows),
            Some((field, rest)) => self.listing(slot, rest).and_then(|parent| parent.nested(field)),
        }
        .map(Rc::new);
        self.listings.borrow_mut().push((slot, Rc::from(key), built.clone()));
        built
    }

    fn leaf(&self, slot: SlotId) -> Leaf<'a> {
        if let Some(Some(leaf)) = self.slots.get(slot as usize) {
            return leaf.clone();
        }
        match self.inputs.get(slot as usize) {
            Some(Some((column, leaf))) => leaf.get_or_init(|| Normalizer::input(*column, self.rows)).clone(),
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

    fn viewed(&mut self, seg: &SegPlan, r: Ref, code: bool) -> (Leaf<'a>, Option<Rc<[u64]>>) {
        let leaf = self.leaf(r.slot);
        let code = code && matches!(leaf.column().values, Values::Text { .. });
        let base = match self.views.iter().find(|(slot, coded, _)| *slot == r.slot && *coded == code) {
            Some((.., view)) => view.clone(),
            None => {
                let view = Normalizer::typed(&leaf, self.rows, code).map(|(leaf, faults)| (leaf, faults.map(Rc::from)));
                self.views.push((r.slot, code, view.clone()));
                view
            }
        };
        let Some((base, faults)) = base else {
            return (self.value(seg, r), None);
        };
        if Self::implied(seg, r) {
            return (base, faults);
        }
        if let Some((.., cached)) = self.viewed.iter().find(|(slot, m, coded, _)| *slot == r.slot && *m == r.present && *coded == code) {
            return (cached.clone(), faults);
        }
        let mask = self.mask(seg, r.present);
        let masked = Leaf::masked(base, &mask);
        self.viewed.push((r.slot, r.present, code, masked.clone()));
        (masked, faults)
    }

    fn fail(&mut self, row: usize, node: &crate::model::DecisionNode, message: Rc<str>) {
        if let Some(slot @ None) = self.errors.get_mut(row) {
            self.failed += 1;
            let known = self.recent.iter().find(|(id, text, _)| Arc::ptr_eq(id, &node.id) && (Rc::ptr_eq(text, &message) || *text == message));
            let error = match known {
                Some((.., error)) => error.clone(),
                None => {
                    let error = Rc::new(EvaluationError::NodeError {
                        node_id: node.id.clone(),
                        source: Box::new(Failure(message.clone())),
                        trace: None,
                    });
                    if self.recent.len() >= Self::RECENT {
                        self.recent.remove(0);
                    }
                    self.recent.push((node.id.clone(), message, error.clone()));
                    error
                }
            };
            *slot = Some(error);
        }
    }

    fn share(&mut self, row: usize, error: &Rc<EvaluationError>) {
        if let Some(slot @ None) = self.errors.get_mut(row) {
            self.failed += 1;
            *slot = Some(error.clone());
        }
    }

    fn fault(&mut self, row: usize, error: NodeError) {
        if let Some(slot @ None) = self.errors.get_mut(row) {
            self.failed += 1;
            *slot = Some(Rc::new(EvaluationError::NodeError {
                node_id: error.node_id,
                source: error.source,
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
        let same = node
            .fields
            .iter()
            .enumerate()
            .map(|(at, (_, child))| match child.fields.is_empty() {
                true => None,
                false => node.fields[..at].iter().position(|(_, other)| other == child),
            })
            .collect();
        Shaped { leaf, obj, fields, same }
    }

    fn objects(shape: &Shaped<'a>, rows: usize) -> Vec<Variable> {
        let mut columns: Vec<Vec<Variable>> = Vec::with_capacity(shape.fields.len());
        for (at, (_, child)) in shape.fields.iter().enumerate() {
            let column = match shape.same.get(at).copied().flatten().and_then(|earlier| columns.get(earlier)) {
                Some(earlier) => earlier.clone(),
                None => Self::objects(child, rows),
            };
            columns.push(column);
        }
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

struct Host<'r> {
    content: &'r Arc<GraphContent>,
    extensions: &'r NodeHandlerExtensions,
    max_depth: u8,
}

struct Run<'r, 'a> {
    graph: &'r CompiledGraph,
    layout: &'r Layout,
    runner: LaneRunner,
    host: Host<'r>,
    store: Store<'a>,
    settled: bool,
    failures: Vec<(usize, usize)>,
    shreds: Vec<(Leaf<'a>, Shredded<'a>)>,
}

impl<'a> Run<'_, 'a> {
    fn alive(&self, positions: Option<Rc<[usize]>>, seen: &mut usize) -> Option<Rc<[usize]>> {
        if self.store.failed == *seen {
            return positions;
        }
        *seen = self.store.failed;
        let rows = self.store.rows;
        let errors = &self.store.errors;
        let total = positions.as_ref().map_or(rows, |p| p.len());
        let live = match &positions {
            Some(p) => p.iter().filter(|&&row| errors[row].is_none()).count(),
            None => errors.iter().filter(|error| error.is_none()).count(),
        };
        match (live + live / 7 < total, live == rows) {
            (true, _) => {
                let mut kept = Vec::with_capacity(live);
                match &positions {
                    Some(p) => kept.extend(p.iter().copied().filter(|&row| errors[row].is_none())),
                    None => kept.extend((0..rows).filter(|&row| errors[row].is_none())),
                }
                Some(kept.into())
            }
            (false, true) => None,
            (false, false) => positions,
        }
    }

    async fn walk(&mut self, root: Arc<SegPlan>) -> Result<(), String> {
        let mut stack: Vec<(Arc<SegPlan>, Option<Rc<[usize]>>, Option<(usize, usize)>)> = vec![(root, None, None)];
        let mut children = Vec::new();
        while let Some((seg, positions, parent)) = stack.pop() {
            if let Some(parent) = parent {
                self.store.rewind(parent);
            }
            self.segment(&seg, positions, &mut children).await?;
            let parent = Some((seg.slots.len(), seg.masks.exprs.len()));
            stack.extend(children.drain(..).rev().map(|(child, members)| (child, members, parent)));
        }
        Ok(())
    }

    async fn segment(&mut self, seg: &SegPlan, positions: Option<Rc<[usize]>>, children: &mut Vec<(Arc<SegPlan>, Option<Rc<[usize]>>)>) -> Result<(), String> {
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
                    if matches!(lane.program, Program::Expression) {
                        self.store.counts.1 += 1;
                    }
                    self.lane(seg, lane, positions, true);
                }
                Op::Table(table) => {
                    self.store.counts.1 += 1;
                    self.table(seg, table, positions);
                }
                Op::Typed { op, units } => self.typed(seg, op, units, positions),
                Op::Deferred { op, units } => self.deferred(seg, op, units, positions),
                Op::Rows(op) => self.rows(seg, op, positions).await,
                Op::Loop { node, list, out } => self.looped(seg, *node, *list, *out, positions),
                Op::Join { inputs, out, strict, ending } => {
                    let leaves: Vec<Leaf<'a>> = inputs.iter().map(|r| self.store.leaf(r.slot)).collect();
                    let mut values = vec![Variable::Null; self.store.rows];
                    let mut parts = Vec::with_capacity(leaves.len());
                    let mut arrays = false;
                    let at: Box<dyn Iterator<Item = usize>> = match positions {
                        Some(positions) => Box::new(positions.iter().copied()),
                        None => Box::new(0..self.store.rows),
                    };
                    for row in at {
                        parts.clear();
                        parts.extend(leaves.iter().map(|leaf| leaf.get(row)));
                        let value = match ending {
                            true => GraphWalker::merge_ending(parts.iter()),
                            false => GraphWalker::merge_values(parts.iter()),
                        };
                        arrays |= matches!(value, Variable::Array(_));
                        values[row] = value;
                    }
                    if *strict && arrays {
                        return Err("array input".into());
                    }
                    self.store.slots[*out as usize] = Some(Leaf::Any(values.into()));
                }
                Op::Select { a, b, take, out } => {
                    let (a, b) = (self.store.value(seg, *a), self.store.value(seg, *b));
                    let take = self.store.mask(seg, *take);
                    if Bits::none(&take) || Rc::ptr_eq(&take, &self.store.ones) || *take == *self.store.ones {
                        let chosen = match Bits::none(&take) {
                            true => a,
                            false => b,
                        };
                        self.store.slots[*out as usize] = Some(chosen);
                        continue;
                    }
                    self.store.slots[*out as usize] = Some(Self::chosen(&a, &b, &take, self.store.rows));
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
        if self.store.failed != seen {
            live = self.alive(live, &mut seen);
            positions = live.as_ref();
        }
        match &seg.end {
            SegEnd::Finish(refs) if matches!(refs.as_slice(), [(path, _)] if path.is_empty()) => {
                let leaves = self.flattened(refs[0].1, positions);
                self.store.groups.push((positions.cloned(), leaves));
                Ok(())
            }
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
            SegEnd::Switch { first, conditions, .. } => {
                let rows = self.store.rows;
                let truths: Vec<Option<Rc<[u64]>>> = conditions
                    .iter()
                    .map(|condition| match condition {
                        Cond::Always => Some(self.store.ones.clone()),
                        Cond::Never => None,
                        Cond::Rows(op) => Some(self.rows_condition(seg, op, positions).into()),
                        Cond::Lane(op) => {
                            let uniform = op.uniform;
                            let first: Rc<[usize]>;
                            let local = match uniform {
                                true => {
                                    first = Rc::from([positions.and_then(|p| p.first().copied()).unwrap_or(0)]);
                                    Some(&first)
                                }
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
                let ones = self.store.ones.clone();
                let owned: Vec<u64>;
                let reach_bits: &[u64] = match positions {
                    None => &ones,
                    Some(p) => {
                        let mut bits = vec![0u64; rows.div_ceil(64)];
                        p.iter().for_each(|&row| Bits::set(&mut bits, row, true));
                        owned = bits;
                        &owned
                    }
                };
                let uniform = truths.iter().all(|t| t.as_ref().is_none_or(|t| Rc::ptr_eq(t, &ones)));
                let mut keys: Vec<u64> = Vec::with_capacity(words * (conditions.len() + 1));
                let mut sets: Vec<Vec<usize>> = Vec::with_capacity(conditions.len() + 1);
                let members = |bits: &[u64]| -> Vec<usize> {
                    let mut found = Vec::with_capacity(bits.iter().map(|word| word.count_ones() as usize).sum());
                    found.extend(
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
                            }),
                    );
                    found
                };
                if uniform || *first {
                    let mut remaining = reach_bits.to_vec();
                    let mut hits = vec![0u64; remaining.len()];
                    for (index, truth) in truths.iter().enumerate() {
                        let Some(truth) = truth else {
                            continue;
                        };
                        let hit = &mut hits[..remaining.len().min(truth.len())];
                        hit.iter_mut().zip(remaining.iter().zip(truth.iter())).for_each(|(h, (r, t))| *h = r & t);
                        if Bits::none(hit) {
                            continue;
                        }
                        let at = keys.len();
                        keys.resize(at + words, 0);
                        if uniform && !*first {
                            for (i, t) in truths.iter().enumerate() {
                                if t.is_some() {
                                    keys[at + i / 64] |= 1 << (i % 64);
                                }
                            }
                            sets.push(members(hit));
                            remaining.fill(0);
                            break;
                        }
                        keys[at + index / 64] |= 1 << (index % 64);
                        remaining.iter_mut().zip(hit.iter()).for_each(|(r, h)| *r &= !h);
                        sets.push(match *hit == *reach_bits {
                            true => Vec::new(),
                            false => members(hit),
                        });
                    }
                    if !Bits::none(&remaining) {
                        keys.resize(keys.len() + words, 0);
                        sets.push(match remaining.as_slice() == reach_bits {
                            true => Vec::new(),
                            false => members(&remaining),
                        });
                    }
                } else if words == 1 {
                    let mut last = 0usize;
                    let mut visit = |row: usize| {
                        let key = truths
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| t.as_ref().is_some_and(|t| Bits::get(t, row)))
                            .fold(0u64, |k, (index, _)| k | 1 << index);
                        let at = match keys.get(last).is_some_and(|k| *k == key) {
                            true => last,
                            false => match keys.iter().position(|k| *k == key) {
                                Some(g) => g,
                                None => {
                                    keys.push(key);
                                    sets.push(Vec::new());
                                    keys.len() - 1
                                }
                            },
                        };
                        sets[at].push(row);
                        last = at;
                    };
                    match positions {
                        Some(p) => p.iter().for_each(|&row| visit(row)),
                        None => (0..rows).for_each(&mut visit),
                    }
                } else {
                    let mut key = vec![0u64; words];
                    let mut last: Option<usize> = None;
                    let mut visit = |row: usize| {
                        key.fill(0);
                        for (index, truth) in truths.iter().enumerate() {
                            if truth.as_ref().is_some_and(|t| Bits::get(t, row)) {
                                key[index / 64] |= 1 << (index % 64);
                            }
                        }
                        let at = match last.filter(|&g| keys.get(g * words..(g + 1) * words) == Some(key.as_slice())) {
                            Some(g) => g,
                            None => match keys.chunks_exact(words).position(|k| k == key.as_slice()) {
                                Some(g) => g,
                                None => {
                                    keys.extend_from_slice(&key);
                                    sets.push(Vec::new());
                                    sets.len() - 1
                                }
                            },
                        };
                        sets[at].push(row);
                        last = Some(at);
                    };
                    match positions {
                        Some(p) => p.iter().for_each(|&row| visit(row)),
                        None => (0..rows).for_each(&mut visit),
                    }
                }
                for (key, members) in keys.chunks_exact(words).zip(sets) {
                    let child = seg.child(self.graph, self.layout, key);
                    let child = child.as_ref().as_ref().map_err(String::clone)?;
                    let members: Option<Rc<[usize]>> = match (members.is_empty(), members.len() == rows) {
                        (true, _) => positions.cloned(),
                        (false, true) => None,
                        (false, false) => Some(members.into()),
                    };
                    children.push((child.clone(), members));
                }
                Ok(())
            }
        }
    }

    fn chosen(a: &Leaf<'a>, b: &Leaf<'a>, take: &[u64], rows: usize) -> Leaf<'a> {
        let (a, b) = (a.column(), b.column());
        let array = Self::selected(&a, &b, take, rows).unwrap_or_else(|| {
            let mut builder = ColumnBuilder::with_capacity(rows);
            for row in 0..rows {
                match Bits::get(take, row) {
                    true => builder.push_cell(&b, row),
                    false => builder.push_cell(&a, row),
                }
            }
            builder.finish()
        });
        Leaf::typed(array)
    }

    fn unit(&mut self, seg: &SegPlan, op: &Op, positions: Option<&Rc<[usize]>>) {
        match op {
            Op::Lane(lane) => self.lane(seg, lane, positions, true),
            Op::Table(table) => self.table(seg, table, positions),
            _ => {}
        }
    }

    fn deferred(&mut self, seg: &SegPlan, op: &Deferred, units: &[Op], positions: Option<&Rc<[usize]>>) {
        let rows = self.store.rows;
        let seen: Vec<(SlotId, Seen)> = op.keys.iter().map(|slot| (*slot, Normalizer::seen(&self.store.leaf(*slot), rows))).collect();
        let key: Vec<Seen> = seen.iter().map(|(_, seen)| *seen).collect();
        let graph = self.graph;
        let store = &self.store;
        let lister = |slot: SlotId, chain: &[&str]| store.listing(slot, chain).map(|listing| listing.classes());
        let pieces = op.variant(&key, units.len(), || {
            Grouping {
                graph,
                masks: &seg.masks,
                slots: &seg.slots,
                seen: &seen,
                listing: Some(&lister),
            }
            .pieces(units)
        });
        let mut at = 0;
        let mut seen = self.store.failed;
        let mut live = positions.cloned();
        for piece in pieces.iter() {
            if self.store.failed != seen {
                live = self.alive(live, &mut seen);
            }
            match piece {
                Piece::Unit => {
                    if let Some(unit) = units.get(at) {
                        self.store.counts.1 += 1;
                        self.unit(seg, unit, live.as_ref());
                    }
                    at += 1;
                }
                Piece::Typed(op, n) => {
                    self.typed(seg, op, units.get(at..at + n).unwrap_or_default(), live.as_ref());
                    at += n;
                }
                Piece::Deferred(op, n) => {
                    self.deferred(seg, op, units.get(at..at + n).unwrap_or_default(), live.as_ref());
                    at += n;
                }
            }
        }
    }

    fn typed(&mut self, seg: &SegPlan, op: &TypedOp, units: &[Op], positions: Option<&Rc<[usize]>>) {
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        if local * 4 < rows {
            self.laned(seg, units, positions);
            return;
        }
        let path = op.cost.path();
        let start = Instant::now();
        let typed = match path.0 {
            Path::Typed => self.lowered(seg, op, positions).map(|lowered| self.merged(seg, op, units, positions, lowered)).is_some(),
            Path::Lane => false,
        };
        if !typed {
            self.laned(seg, units, positions);
        }
        op.cost.record(path, start.elapsed(), local);
    }

    fn laned(&mut self, seg: &SegPlan, units: &[Op], positions: Option<&Rc<[usize]>>) {
        self.store.counts.1 += units.len();
        let mut seen = self.store.failed;
        let mut live = positions.cloned();
        for unit in units {
            if self.store.failed != seen {
                live = self.alive(live, &mut seen);
            }
            self.unit(seg, unit, live.as_ref());
        }
    }

    fn merged(&mut self, seg: &SegPlan, op: &TypedOp, units: &[Op], positions: Option<&Rc<[usize]>>, (mut out, keyed, select, prefault): Lowered) {
        let rows = self.store.rows;
        let local = positions.map_or(rows, |p| p.len());
        self.store.counts.0 += units.len();
        let ids = keyed.as_deref();
        let words = rows.div_ceil(64);
        let mut take = vec![0u64; words];
        if let Some(prefault) = prefault {
            match positions {
                Some(positions) => positions.iter().filter(|&&row| Bits::get(&prefault, row)).for_each(|&row| Bits::set(&mut take, row, true)),
                None => take.iter_mut().zip(&prefault).for_each(|(t, f)| *t |= f),
            }
        }
        let mut faulted = !Bits::none(&take);
        let mut dicts = op.exports.dictionaries();
        let mut at = 0;
        for (unit, targets) in units.iter().zip(&op.targets) {
            let slots = out.slots().get(at..at + targets.len()).unwrap_or_default();
            if slots.iter().any(|slot| slot.fault.any()) {
                let mut failed: Vec<u64> = Vec::new();
                for slot in slots.iter().filter(|slot| slot.fault.any()) {
                    let words = slot.fault.words();
                    failed.resize(failed.len().max(words.len()), 0);
                    failed.iter_mut().zip(words).for_each(|(f, w)| *f |= w);
                }
                match ids {
                    Some(ids) => (0..local).filter(|&at| Bits::get(&failed, ids[at] as usize)).for_each(|at| Bits::set(&mut take, positions.map_or(at, |p| p[at]), true)),
                    None => take.iter_mut().zip(&failed).for_each(|(t, f)| *t |= f),
                }
                Bits::trim(&mut take, rows);
                faulted = true;
            }
            if faulted {
                let rerun: Vec<usize> = Bits::rows(&take).filter(|&row| self.store.errors[row].is_none()).collect();
                take.iter_mut().for_each(|word| *word = 0);
                if !rerun.is_empty() {
                    let rerun: Rc<[usize]> = rerun.into();
                    self.settled = false;
                    self.unit(seg, unit, Some(&rerun));
                    self.settled = true;
                    rerun.iter().filter(|&&row| self.store.errors[row].is_none()).for_each(|&row| Bits::set(&mut take, row, true));
                }
                faulted = !Bits::none(&take);
            }
            let mut leaves: Vec<(SlotId, Leaf<'a>)> = Vec::with_capacity(targets.len());
            let mut masks: Vec<(M, Rc<[u64]>)> = Vec::new();
            for target in targets {
                let Some(slot) = out.slot_mut(at) else {
                    return;
                };
                at += 1;
                let listed = slot.offsets.is_some();
                if let (Some(select), false) = (&select, listed) {
                    slot.valid.resize(select.len(), 0);
                    slot.valid.iter_mut().zip(select).for_each(|(valid, chosen)| *valid &= chosen);
                }
                match (target, unit) {
                    (Target::Slot(id), _) if listed => {
                        let leaf = op.exports.leaf(slot, &mut dicts);
                        let leaf = match &select {
                            Some(select) => Leaf::masked(leaf, &Rc::from(select.as_slice())),
                            None => leaf,
                        };
                        leaves.push((*id, leaf));
                    }
                    (Target::Slot(id), _) => leaves.push((*id, op.exports.leaf(slot, &mut dicts))),
                    (Target::Choice, Op::Table(table)) => self.choice(table, slot, &mut leaves, &mut masks),
                    (Target::Choice, _) => {}
                }
            }
            for (id, leaf) in leaves {
                let leaf = self.widen(leaf, ids, positions);
                let leaf = match faulted {
                    false => leaf,
                    true => {
                        let located: Rc<[(usize, usize)]> = (0..rows).map(|row| (usize::from(Bits::get(&take, row)), row)).collect();
                        Leaf::stitched(vec![leaf, self.store.leaf(id)], located)
                    }
                };
                if let Some(entry) = self.store.slots.get_mut(id as usize) {
                    *entry = Some(leaf);
                }
            }
            for (m, bits) in masks {
                let mut bits: Vec<u64> = match ids {
                    Some(ids) => {
                        let local: Rc<[u64]> = Bits::of(local, |at| Bits::get(&bits, ids[at] as usize)).into();
                        self.store.spread(&local, positions).to_vec()
                    }
                    None => bits.to_vec(),
                };
                bits.resize(rows.div_ceil(64), 0);
                Bits::trim(&mut bits, rows);
                if faulted {
                    let laned = self.store.mask(seg, m);
                    bits.iter_mut().zip(laned.iter().zip(&take)).for_each(|(bit, (lane, fault))| *bit = (*bit & !fault) | (lane & fault));
                }
                self.store.produce(m, bits.into());
            }
        }
    }

    fn decision(&self, op: &TableOp) -> Option<&Table> {
        match self.graph.step_at(op.node).map(|step| &step.kind) {
            Some(Kind::Node { body: Body::Table(table), .. }) => Some(table),
            _ => None,
        }
    }

    fn choice(&self, op: &TableOp, slot: &Slot, leaves: &mut Vec<(SlotId, Leaf<'a>)>, masks: &mut Vec<(M, Rc<[u64]>)>) {
        let Some(table) = self.decision(op) else {
            return;
        };
        let count = slot.len;
        let rules = table.rules();
        let codes: Vec<i32> = match &slot.data {
            Slotted::Num { mant, .. } => (0..count)
                .map(|at| match (slot.is_valid(at), mant.get(at).and_then(|m| usize::try_from(*m).ok())) {
                    (true, Some(rule)) if rule < rules => rule as i32,
                    _ => -1,
                })
                .collect(),
            _ => vec![-1; count],
        };
        let matched: Rc<[u64]> = Bits::of(count, |at| codes[at] >= 0).into();
        match table.literal() {
            true => {
                let (columns, present, _) = table.columns(First::coded(rules, codes));
                for ((id, leaf), (m, mask)) in op.outs.iter().zip(columns).zip(op.present.iter().zip(present)) {
                    leaves.push((*id, leaf));
                    masks.push((*m, mask.dense(count)));
                }
            }
            false => {
                for (index, m) in op.present.iter().enumerate() {
                    let bits: Rc<[u64]> = Bits::of(count, |at| usize::try_from(codes[at]).is_ok_and(|rule| table.writes(rule, index))).into();
                    masks.push((*m, bits));
                }
            }
        }
        masks.push((op.matched, matched));
    }

    fn widen(&self, leaf: Leaf<'a>, ids: Option<&[u32]>, positions: Option<&Rc<[usize]>>) -> Leaf<'a> {
        match ids {
            Some(ids) => self.store.place(leaf.expand(ids), positions),
            None => leaf,
        }
    }

    fn lowered(&mut self, seg: &SegPlan, op: &TypedOp, positions: Option<&Rc<[usize]>>) -> Option<Lowered> {
        let rows = self.store.rows;
        let mut hints: Vec<Option<(Rc<[i32]>, usize)>> = op
            .columns
            .iter()
            .map(|read| self.store.hints.iter().find(|(slot, _, _)| *slot == read.slot).map(|(_, codes, size)| (codes.clone(), *size)))
            .collect();
        let long = op.program.instructions() >= TypedOp::LONG * op.columns.len();
        let attempt = op.sparse.load(Ordering::Relaxed) == 0 && op.lists.is_empty();
        let raw: Vec<Leaf<'a>> = op.columns.iter().map(|read| self.store.leaf(read.slot)).collect();
        let keyed = attempt
            && raw.iter().zip(&hints).all(|(leaf, hint)| {
                hint.is_some()
                    || match leaf.column().values {
                        Values::Dict { .. } | Values::Bool { .. } => true,
                        Values::Scaled { .. } | Values::I64(_) | Values::Dec(_) | Values::Text { .. } => long,
                        _ => false,
                    }
            });
        let mut prefault: Option<Vec<u64>> = None;
        let mut leaves: Vec<Leaf<'a>> = Vec::with_capacity(op.columns.len());
        for (at, read) in op.columns.iter().enumerate() {
            let (leaf, faults) = self.store.viewed(seg, *read, keyed || op.shared.get(at).copied().unwrap_or(false));
            if let Some(faults) = faults {
                let bits = prefault.get_or_insert_with(|| vec![0u64; rows.div_ceil(64)]);
                bits.iter_mut().zip(faults.iter()).for_each(|(bit, fault)| *bit |= fault);
            }
            leaves.push(leaf);
        }
        let distinct = match keyed {
            true => {
                for (leaf, hint) in leaves.iter().zip(hints.iter_mut()).filter(|(_, hint)| hint.is_none()) {
                    *hint = Self::numbered(&leaf.column(), rows);
                }
                let distinct = Self::distinct(&leaves, &hints, positions, rows);
                if distinct.is_none() {
                    op.sparse.store(TypedOp::SPARSE, Ordering::Relaxed);
                }
                distinct
            }
            false => {
                let _ = op.sparse.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1));
                None
            }
        };
        let mut out = op.program.outputs();
        match distinct {
            Some((reps, ids)) => {
                let picks: Rc<[usize]> = reps.into();
                let picked: Vec<Leaf<'a>> = leaves.iter().map(|leaf| leaf.pick(&picks)).collect();
                let mut batch = Batch::new(picks.len());
                picked.iter().for_each(|leaf| batch.push_column(leaf.column()));
                op.program.run(&batch, &mut out, None).ok()?;
                Some((out, Some(ids), None, prefault))
            }
            None => {
                let select: Option<Vec<u64>> = positions.map(|positions| {
                    let mut bits = vec![0u64; rows.div_ceil(64)];
                    positions.iter().for_each(|&row| Bits::set(&mut bits, row, true));
                    bits
                });
                let mut listed: Vec<Prepared<'a>> = Vec::with_capacity(op.lists.len());
                for (read, chain) in &op.lists {
                    let chain: Vec<&str> = chain.iter().map(|field| field.as_ref()).collect();
                    let listing = self.store.listing(read.slot, &chain)?;
                    let (validity, faults) = match (chain.is_empty(), Store::implied(seg, *read)) {
                        (true, false) => {
                            let mask = self.store.mask(seg, read.present);
                            let mut validity = listing.validity.as_deref().map_or_else(|| Bits::ones(rows), <[u64]>::to_vec);
                            let mut faults = listing.hosted.as_deref().map_or_else(|| vec![0u64; rows.div_ceil(64)], <[u64]>::to_vec);
                            for row in (0..rows).filter(|&row| !Bits::get(&mask, row)) {
                                match listing.offsets[row] == listing.offsets[row + 1] {
                                    true => Bits::set(&mut validity, row, false),
                                    false => Bits::set(&mut faults, row, true),
                                }
                            }
                            (Some(validity), Some(faults))
                        }
                        _ => (None, None),
                    };
                    listed.push((listing, validity, faults));
                }
                let mut batch = Batch::new(rows);
                leaves.iter().for_each(|leaf| batch.push_column(leaf.column()));
                for ((listing, validity, faults), def) in listed.iter().zip(&op.program.hir().schema.lists) {
                    let validity = validity.as_deref().or(listing.validity.as_deref()).map(|bits| (bits, 0));
                    let list = zen_expression::typed::List::new(&listing.offsets, validity, listing.columns(&def.fields)?);
                    batch.push_list(match faults.as_deref().or(listing.hosted.as_deref()) {
                        Some(bits) => list.faults(bits, 0),
                        None => list,
                    });
                }
                op.program.run(&batch, &mut out, select.as_deref()).ok()?;
                Some((out, None, select, prefault))
            }
        }
    }

    fn numbered(column: &Column, rows: usize) -> Option<(Rc<[i32]>, usize)> {
        const SLOTS: usize = 256;
        const LIMIT: usize = 128;
        let (mant, scale) = match column.values {
            Values::Scaled { mant, scale } => (mant.get(..rows)?, Some(scale.get(..rows)?)),
            Values::I64(values) => (values.get(..rows)?, None),
            _ => return None,
        };
        let mut table = [(0i64, 0u8, u8::MAX); SLOTS];
        let mut count = 0usize;
        let mut codes: Vec<i32> = Vec::with_capacity(rows);
        let mut last: Option<(i64, u8, i32)> = None;
        for (at, &m) in mant.iter().enumerate() {
            let s = scale.map_or(0, |scale| scale[at]);
            if let Some((_, _, code)) = last.filter(|(lm, ls, _)| *lm == m && *ls == s) {
                codes.push(code);
                continue;
            }
            let mut slot = ((m as u64 ^ u64::from(s) << 56).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as usize;
            let code = loop {
                let entry = &mut table[slot];
                if entry.2 == u8::MAX {
                    if count >= LIMIT {
                        return None;
                    }
                    *entry = (m, s, count as u8);
                    count += 1;
                    break i32::from(entry.2);
                }
                if entry.0 == m && entry.1 == s {
                    break i32::from(entry.2);
                }
                slot = (slot + 1) % SLOTS;
            };
            last = Some((m, s, code));
            codes.push(code);
        }
        Some((codes.into(), count))
    }

    fn looped(&mut self, seg: &SegPlan, node: petgraph::prelude::NodeIndex, list: Ref, out: SlotId, positions: Option<&Rc<[usize]>>) {
        let graph = self.graph;
        let Some((step, Kind::Node { body, transform })) = graph.step_at(node).map(|step| (step, &step.kind)) else {
            return;
        };
        let leaf = self.store.value(seg, list);
        let rows = self.store.rows;
        let mut at: Vec<usize> = match positions {
            Some(positions) => positions.to_vec(),
            None => (0..rows).collect(),
        };
        let mut native: Option<(Leaf<'a>, Vec<usize>)> = None;
        match leaf.items() {
            Some(items) => {
                if self.native_loop(step, body, transform.pass_through, &items, out, &at) {
                    return;
                }
            }
            None => {
                if let Some((records, odd)) = self.shredded(&leaf, rows) {
                    let (rest, lifted): (Vec<usize>, Vec<usize>) = at.iter().partition(|&&row| Bits::get(&odd, row));
                    if self.native_loop(step, body, transform.pass_through, &Items::Records(records), out, &lifted) {
                        if rest.is_empty() {
                            return;
                        }
                        if let Some(leaf) = self.store.slots[out as usize].take() {
                            native = Some((leaf, lifted));
                            at = rest;
                        }
                    }
                }
            }
        }
        let mut items: Vec<Variable> = Vec::new();
        let mut spans: Vec<(usize, usize, usize)> = Vec::with_capacity(at.len());
        for &row in &at {
            match leaf.get(row).as_array() {
                Some(array) => {
                    let start = items.len();
                    items.extend(array.borrow().iter().cloned());
                    spans.push((row, start, items.len()));
                }
                None => self.store.fail(row, &step.node, Rc::from("Expected an array")),
            }
        }
        let produced = CompiledGraph::items(&mut self.runner, step, body, &items, &[]);
        let mut values = vec![Variable::Null; rows];
        for (row, start, end) in spans {
            let mut collected = Vec::with_capacity(end - start);
            let mut failure = None;
            for (item, output) in items[start..end].iter().zip(&produced[start..end]) {
                match output {
                    Ok(out) => collected.push(match transform.pass_through {
                        true => item.clone().merge_clone(out),
                        false => out.clone(),
                    }),
                    Err(e) => {
                        failure = Some(e.clone());
                        break;
                    }
                }
            }
            match failure {
                Some(e) => self.store.fail(row, &step.node, Rc::from(e)),
                None => values[row] = Variable::from_array(collected),
            }
        }
        if let Some((leaf, lifted)) = native {
            lifted.into_iter().for_each(|row| values[row] = leaf.get(row));
        }
        self.store.slots[out as usize] = Some(Leaf::Any(values.into()));
    }

    fn shredded(&mut self, leaf: &Leaf<'a>, rows: usize) -> Shredded<'a> {
        if let Some((_, cached)) = self.shreds.iter().find(|(seen, _)| seen.same(leaf)) {
            return cached.clone();
        }
        let shredded = Self::shred(leaf, rows);
        self.shreds.push((leaf.clone(), shredded.clone()));
        shredded
    }

    fn shred(leaf: &Leaf<'a>, rows: usize) -> Shredded<'a> {
        let column = leaf.column();
        let shred = Shred::of(&column, rows)?;
        let mut odd = vec![0u64; rows.div_ceil(64)];
        shred.hosted.iter().filter(|&&row| leaf.get(row).as_array().is_some()).for_each(|&row| Bits::set(&mut odd, row, true));
        let valid = match shred.validity {
            Some(bits) => bits.to_vec(),
            None => Bits::ones(rows),
        };
        let count = shred.children;
        let fields = shred
            .fields
            .into_iter()
            .map(|(name, array)| {
                let present: Rc<[u64]> = match array.validity_bits() {
                    Some(bits) => Bits::window(bits, 0, count).into(),
                    None => Bits::ones(count).into(),
                };
                (Symbol::from(name.as_ref()), Leaf::typed(array), present)
            })
            .collect();
        Some((Rc::new(Records::new(rows, shred.offsets.to_vec(), valid, fields, None)), odd.into()))
    }

    fn native_loop(&mut self, step: &crate::compiled::Step, body: &Body, pass_through: bool, source: &Items<'a>, out: SlotId, at: &[usize]) -> bool {
        let Some(keys) = CompiledGraph::item_keys(step, body) else {
            return false;
        };
        let Some(fields) = source.fields() else {
            return false;
        };
        let scalar = |leaf: &Leaf| !matches!(leaf.column().values, Values::Any(_) | Values::List { .. } | Values::Struct { .. });
        let flat = fields.iter().all(|(name, leaf, _)| !name.is_empty() && !name.contains('.') && (scalar(leaf) || !keys.iter().any(|k| k.strip_prefix(name.as_str()).is_some_and(|r| r.starts_with('.')))));
        if !flat {
            return false;
        }
        let rows = self.store.rows;
        let mut picked: Vec<usize> = Vec::new();
        let mut spans: Vec<(usize, usize, usize)> = Vec::with_capacity(at.len());
        let mut missing: Vec<usize> = Vec::new();
        for &row in at {
            match source.range(row) {
                Some((a, b)) => {
                    let start = picked.len();
                    picked.extend(a..b);
                    spans.push((row, start, picked.len()));
                }
                None => missing.push(row),
            }
        }
        let count = picked.len();
        let identity = count == source.count() && picked.iter().enumerate().all(|(i, p)| i == *p);
        let picks: Rc<[usize]> = picked.into();
        let local = |leaf: &Leaf<'a>, present: &Rc<[u64]>| -> (Leaf<'a>, Rc<[u64]>) {
            match identity {
                true => (leaf.clone(), present.clone()),
                false => (leaf.pick(&picks), Bits::gather(present, &picks).into()),
            }
        };
        let fields: Vec<Field<'a>> = fields
            .iter()
            .map(|(name, leaf, present)| {
                let (leaf, present) = local(leaf, present);
                (name.clone(), leaf, present)
            })
            .collect();
        let mut leaves = Vec::with_capacity(keys.len());
        let mut present = Vec::with_capacity(keys.len());
        for key in &keys {
            match fields.iter().find(|(name, _, _)| name.as_str() == key.as_ref()) {
                Some((_, leaf, bits)) => {
                    leaves.push(leaf.clone());
                    present.push(Mask::Bits(bits.clone()));
                }
                None => {
                    leaves.push(Leaf::nulls(count));
                    present.push(Mask::Bits(vec![0u64; count.div_ceil(64)].into()));
                }
            }
        }
        let items: Rc<[usize]> = (0..count).collect();
        let data = Data::record(&items, Layer::new(keys.into(), leaves, Some(present)), None);
        let (output, failures) = CompiledGraph::item_run(&mut self.runner, step, body, &data);
        let paths: Vec<(Arc<str>, Leaf<'a>)> = output.leaves().to_vec();
        let shaped = matches!(output.shape, DataShape::Patched { base: None, .. })
            && paths.iter().all(|(path, _)| !path.is_empty() && !path.split('.').any(str::is_empty))
            && !paths.iter().any(|(a, _)| paths.iter().any(|(b, _)| b.strip_prefix(a.as_ref()).is_some_and(|r| r.starts_with('.'))));
        let nulled: Option<Rc<[u64]>> = match (&output.shape, pass_through) {
            (DataShape::Patched { nulls: Some(nulls), .. }, false) => Some(nulls.clone()),
            _ => None,
        };
        let mut owner = vec![0usize; count];
        for &(row, start, end) in &spans {
            owner[start..end].iter_mut().for_each(|o| *o = row);
        }
        let mut failed = vec![false; rows];
        let mut failures = failures;
        failures.sort_by_key(|(item, _)| *item);
        for (item, message) in failures {
            let Some(&row) = owner.get(item) else {
                continue;
            };
            if !failed[row] {
                failed[row] = true;
                self.store.fail(row, &step.node, Rc::from(message));
            }
        }
        for row in missing {
            self.store.fail(row, &step.node, Rc::from("Expected an array"));
        }
        let merged: Option<Vec<Field<'a>>> = match (shaped, pass_through) {
            (false, _) => None,
            (true, false) => Some(
                paths
                    .iter()
                    .map(|(path, _)| {
                        let (leaf, bits) = output.present(path);
                        (Symbol::from(path.as_ref()), leaf, bits.into())
                    })
                    .collect(),
            ),
            (true, true) => Self::passed(&fields, &output, &paths, count, &scalar),
        };
        let mut offsets = Vec::with_capacity(rows + 1);
        let mut valid = vec![0u64; rows.div_ceil(64)];
        let mut next = spans.iter().peekable();
        let mut total = 0i32;
        offsets.push(0);
        for row in 0..rows {
            if let Some(&&(span_row, start, end)) = next.peek() {
                if span_row == row {
                    total += (end - start) as i32;
                    if !failed[row] {
                        Bits::set(&mut valid, row, true);
                    }
                    next.next();
                }
            }
            offsets.push(total);
        }
        let leaf = match merged {
            Some(fields) => Leaf::Records(Rc::new(Records::new(rows, offsets, valid, fields, nulled))),
            None => {
                let values = output.materialized();
                let mut arrays = vec![Variable::Null; rows];
                for &(row, start, end) in &spans {
                    if failed[row] {
                        continue;
                    }
                    let collected: Vec<Variable> = (start..end)
                        .map(|item| {
                            let value = values.get(item).cloned().unwrap_or(Variable::Null);
                            match pass_through {
                                true => Records::object(&fields, item).merge_clone(&value),
                                false => value,
                            }
                        })
                        .collect();
                    arrays[row] = Variable::from_array(collected);
                }
                Leaf::Any(arrays.into())
            }
        };
        self.store.slots[out as usize] = Some(leaf);
        true
    }

    fn passed(fields: &[Field<'a>], output: &Data<'a>, paths: &[(Arc<str>, Leaf<'a>)], count: usize, scalar: &dyn Fn(&Leaf) -> bool) -> Option<Vec<Field<'a>>> {
        let mut merged: Vec<Field<'a>> = fields.to_vec();
        for (path, _) in paths {
            let (leaf, bits) = output.present(path);
            if let Some((top, _)) = path.split_once('.') {
                if fields.iter().any(|(name, _, _)| name.as_str() == top) {
                    return None;
                }
                merged.push((Symbol::from(path.as_ref()), leaf, bits.into()));
                continue;
            }
            let valued: Vec<u64> = leaf.valued(count).iter().zip(&bits).map(|(v, b)| v & b).collect();
            match merged.iter().position(|(name, _, _)| name.as_str() == path.as_ref()) {
                None => merged.push((Symbol::from(path.as_ref()), leaf, valued.into())),
                Some(at) => {
                    let (name, item, present) = merged[at].clone();
                    if !scalar(&item) {
                        return None;
                    }
                    let (patch, base) = (leaf.column(), item.column());
                    let mut builder = ColumnBuilder::with_capacity(count);
                    let mut kept = vec![0u64; count.div_ceil(64)];
                    for i in 0..count {
                        match (Bits::get(&valued, i), Bits::get(&bits, i), Bits::get(&present, i)) {
                            (true, _, _) => {
                                builder.push_cell(&patch, i);
                                Bits::set(&mut kept, i, true);
                            }
                            (false, false, true) => {
                                builder.push_cell(&base, i);
                                Bits::set(&mut kept, i, true);
                            }
                            _ => builder.push_null(),
                        }
                    }
                    merged[at] = (name, Leaf::typed(builder.finish()), kept.into());
                }
            }
        }
        Some(merged)
    }

    fn flattened(&mut self, r: Ref, positions: Option<&Rc<[usize]>>) -> Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)> {
        let leaf = self.store.leaf(r.slot);
        let rows = self.store.rows;
        let mut shredder = Shredder::new(rows);
        let at: Box<dyn Iterator<Item = usize>> = match positions {
            Some(positions) => Box::new(positions.iter().copied()),
            None => Box::new(0..rows),
        };
        for row in at {
            if self.store.errors[row].is_some() {
                continue;
            }
            let value = leaf.get(row);
            VariableCleaner::new().clean(&value);
            shredder.visit(&value, 0, row);
        }
        shredder.finish()
    }

    fn materialize(&mut self, seg: &SegPlan, view: &Node) -> Leaf<'a> {
        if let Some(leaf) = view.leaf.filter(|leaf| leaf.present == M::All && view.fields.is_empty()) {
            return self.store.leaf(leaf.slot);
        }
        let shape = self.store.shape(seg, view);
        Leaf::Any(Store::objects(&shape, self.store.rows).into())
    }

    fn layer(&mut self, seg: &SegPlan, node: &Node, prefix: &str, out: &mut Vec<(Arc<str>, Leaf<'a>, Rc<[u64]>)>) -> Option<Vec<u64>> {
        match (node.leaf, node.fields.is_empty()) {
            (Some(_), false) => None,
            (Some(r), true) => {
                let bits = self.store.mask(seg, r.present);
                out.push((Arc::from(prefix), self.store.leaf(r.slot), bits.clone()));
                Some(bits.to_vec())
            }
            (None, empty) => {
                let mut any = vec![0u64; self.store.words];
                for (key, child) in &node.fields {
                    let path = match prefix.is_empty() {
                        true => key.to_string(),
                        false => format!("{prefix}.{key}"),
                    };
                    let bits = self.layer(seg, child, &path, out)?;
                    any.iter_mut().zip(&bits).for_each(|(a, b)| *a |= b);
                }
                let obj = self.store.mask(seg, node.obj.unwrap_or(M::None));
                let agrees = match (empty, prefix.is_empty()) {
                    (_, true) => obj.iter().zip(self.store.ones.iter()).all(|(o, one)| o == one),
                    (true, false) => Bits::none(&obj),
                    (false, false) => obj.as_ref() == any.as_slice(),
                };
                agrees.then_some(any)
            }
        }
    }

    fn columnar(&mut self, seg: &SegPlan, view: &Node, positions: Option<&Rc<[usize]>>) -> Option<Data<'a>> {
        let mut leaves = Vec::new();
        self.layer(seg, view, "", &mut leaves)?;
        if leaves.iter().any(|(path, _, _)| path.is_empty()) {
            return None;
        }
        let rows = positions.map_or(self.store.rows, |p| p.len());
        let mut paths = Vec::with_capacity(leaves.len());
        let mut columns = Vec::with_capacity(leaves.len());
        let mut present = Vec::with_capacity(leaves.len());
        for (path, leaf, bits) in leaves {
            paths.push(path);
            let mask = Mask::Bits(bits);
            match positions {
                Some(positions) => {
                    columns.push(leaf.pick(positions));
                    present.push(mask.pick(positions));
                }
                None => {
                    columns.push(leaf);
                    present.push(mask);
                }
            }
        }
        let local: Rc<[usize]> = (0..rows).collect();
        Some(Data::record(&local, Layer::new(paths.into(), columns, Some(present)), None))
    }

    fn settle(&mut self, results: Vec<Result<Variable, NodeError>>, at: &[usize]) -> Vec<Variable> {
        results
            .into_iter()
            .zip(at)
            .map(|(result, &row)| match result {
                Ok(value) => value,
                Err(error) => {
                    self.store.fault(row, error);
                    Variable::Null
                }
            })
            .collect()
    }

    fn inputs(&mut self, seg: &SegPlan, input: Option<&Node>, nodes: &Option<Vec<(Arc<str>, Ref)>>, positions: Option<&Rc<[usize]>>) -> (Vec<usize>, Vec<Variable>, Vec<Option<Variable>>) {
        let at: Vec<usize> = match positions {
            Some(positions) => positions.to_vec(),
            None => (0..self.store.rows).collect(),
        };
        let inputs = match input {
            Some(view) => {
                let leaf = self.materialize(seg, view);
                at.iter().map(|&row| leaf.get(row)).collect()
            }
            None => Vec::new(),
        };
        let objects = match nodes {
            None => Vec::new(),
            Some(nodes) => {
                let leaves: Vec<(Symbol, Leaf<'a>)> = nodes.iter().map(|(name, r)| (Symbol::from(name.as_ref()), self.store.leaf(r.slot))).collect();
                at.iter()
                    .map(|&row| {
                        let mut map = VariableMap::with_capacity(leaves.len());
                        for (name, leaf) in &leaves {
                            map.insert(name.clone(), leaf.get(row));
                        }
                        Some(Variable::from_object(map))
                    })
                    .collect()
            }
        };
        (at, inputs, objects)
    }

    async fn rows(&mut self, seg: &SegPlan, op: &RowsOp, positions: Option<&Rc<[usize]>>) {
        let graph = self.graph;
        let Some(step) = graph.step_at(op.node) else {
            return;
        };
        let data = match step.kind {
            Kind::Node { .. } | Kind::Output => self.columnar(seg, &op.view, positions),
            _ => None,
        };
        if let (Kind::Output, Some(data)) = (&step.kind, &data) {
            let nodes = self.inputs(seg, None, &op.nodes, positions).2;
            let failures = graph.validate_input(step, data, &nodes, self.host.content, self.host.extensions).await;
            for (i, message) in failures {
                let row = positions.map_or(i, |p| p[i]);
                self.store.fail(row, &step.node, Rc::from(message));
            }
            return;
        }
        if let (Kind::Host, DecisionNodeKind::DecisionNode { content }) = (&step.kind, &step.node.kind) {
            let count = positions.map_or(self.store.rows, |p| p.len());
            if CompiledGraph::batchable(&content.transform_attributes, count) {
                let values = match CompiledGraph::resolve(content, self.host.extensions).await {
                    Err(failure) => {
                        let error = failure.shared(step, &content.key);
                        match positions {
                            Some(positions) => positions.iter().for_each(|&row| self.store.share(row, &error)),
                            None => (0..count).for_each(|row| self.store.share(row, &error)),
                        }
                        vec![Variable::Null; count]
                    }
                    Ok(graph) => {
                        let (at, inputs, nodes) = self.inputs(seg, Some(&op.view), &op.nodes, positions);
                        let results = CompiledGraph::batch(step, content, &graph, &inputs, &nodes, self.host.extensions, self.host.max_depth, 0).await;
                        self.settle(results, &at)
                    }
                };
                let leaf = self.store.place(Leaf::Any(values.into()), positions);
                self.store.slots[op.out as usize] = Some(leaf);
                return;
            }
        }
        let (at, inputs, nodes) = self.inputs(seg, data.is_none().then_some(&op.view), &op.nodes, positions);
        let values: Vec<Variable> = match &step.kind {
            Kind::Host => {
                let results = match &step.node.kind {
                    DecisionNodeKind::CustomNode { content } => CompiledGraph::custom(step, content, inputs, nodes, self.host.extensions).await,
                    _ => CompiledGraph::hosted(step, &inputs, &nodes, self.host.extensions, self.host.max_depth, 0, self.store.rows > 1).await,
                };
                self.settle(results, &at)
            }
            Kind::Input | Kind::Output => {
                let (values, failures) = graph.bounded(step, &inputs, &nodes, self.host.content, self.host.extensions).await;
                for (i, message) in failures {
                    self.store.fail(at[i], &step.node, Rc::from(message));
                }
                values
            }
            _ => {
                let data = data.unwrap_or_else(|| Data::plain((0..inputs.len()).collect(), inputs.into()));
                let (output, failures) = graph.rowwise(&mut self.runner, step, data, &nodes);
                for (i, message) in failures {
                    self.store.fail(at[i], &step.node, Rc::from(message));
                }
                if !op.keys.is_empty() {
                    for (path, slot, present) in &op.keys {
                        let (leaf, bits) = match present {
                            M::All => (output.column_at(path), None),
                            _ => {
                                let (leaf, bits) = output.present(path);
                                (leaf, Some(bits))
                            }
                        };
                        let leaf = self.store.place(leaf, positions);
                        self.store.slots[*slot as usize] = Some(leaf);
                        if let Some(bits) = bits {
                            let bits = self.store.spread(&bits.into(), positions);
                            self.store.produce(*present, bits);
                        }
                    }
                    return;
                }
                output.materialized().to_vec()
            }
        };
        let leaf = self.store.place(Leaf::Any(values.into()), positions);
        self.store.slots[op.out as usize] = Some(leaf);
    }

    fn rows_condition(&mut self, seg: &SegPlan, op: &RowsCond, positions: Option<&Rc<[usize]>>) -> Vec<u64> {
        let mut bits = vec![0u64; self.store.rows.div_ceil(64)];
        let Some(Kind::Switch { conditions, .. }) = self.graph.step_at(op.node).map(|step| &step.kind) else {
            return bits;
        };
        let Some((_, Condition::Program(program))) = conditions.get(op.index) else {
            return bits;
        };
        let view = Node {
            leaf: Some(op.input),
            obj: None,
            fields: Vec::new(),
        };
        let (at, inputs, nodes) = self.inputs(seg, Some(&view), &op.nodes, positions);
        let scopes: Vec<zen_expression::Scope> = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| CompiledGraph::scope(input, nodes.get(i).and_then(Option::as_ref)))
            .collect();
        for (result, &row) in self.runner.evaluate(program, &scopes).into_iter().zip(&at) {
            if matches!(result, Ok(Variable::Bool(true))) {
                Bits::set(&mut bits, row, true);
            }
        }
        bits
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
        let listed: Vec<bool> = leaves.iter().map(|leaf| leaf.records().is_some()).collect();
        if listed.iter().enumerate().any(|(at, &listed)| listed && hints.get(at).is_none_or(Option::is_none)) {
            return None;
        }
        let held: Vec<Option<Vec<u64>>> = leaves
            .iter()
            .zip(&listed)
            .map(|(leaf, &listed)| listed.then(|| leaf.validity(rows)))
            .collect();
        let columns: Vec<Column> = leaves
            .iter()
            .zip(&held)
            .map(|(leaf, held)| match held {
                Some(bits) => Column {
                    values: Values::Any(&[]),
                    validity: Some((bits.as_slice(), 0)),
                },
                None => leaf.column(),
            })
            .collect();
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
        let mut ids = vec![0u32; local];
        for ((at, &key), id) in keys.iter().enumerate().zip(ids.iter_mut()) {
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
            *id = entry.1;
        }
        Some((reps, ids))
    }

    fn lane(&mut self, seg: &SegPlan, op: &LaneOp, positions: Option<&Rc<[usize]>>, report: bool) {
        let Some((program, entries)) = Self::program(self.graph, op) else {
            return;
        };
        let rows = self.store.rows;
        let leaves: Vec<Leaf<'a>> = op.reads.iter().map(|(_, r)| self.store.value(seg, *r)).collect();
        let held: Vec<Option<Rc<Records<'a>>>> = match Records::LANE_VIEW {
            true => leaves.iter().map(Leaf::records).collect(),
            false => Vec::new(),
        };
        let parts: Vec<Option<Vec<(&str, Column)>>> = match held.iter().any(Option::is_some) {
            true => held.iter().map(|r| r.as_ref().map(|r| r.parts())).collect(),
            false => Vec::new(),
        };
        let items: Vec<Option<Column>> = held.iter().zip(&parts).map(|(r, p)| Some(r.as_ref()?.item(p.as_ref()?))).collect();
        let mut columns = Columns::new(rows);
        columns.columns.reserve_exact(op.reads.len());
        for (at, ((key, _), leaf)) in op.reads.iter().zip(&leaves).enumerate() {
            let view = match (held.get(at).and_then(Option::as_ref), items.get(at).and_then(Option::as_ref)) {
                (Some(record), Some(item)) => record.list(item),
                _ => leaf.column(),
            };
            columns = columns.column(key.as_ref(), view);
        }
        let program = match (op.specialized.get(), self.settled) {
            (Some(special), _) => special.as_ref().unwrap_or(program),
            (None, true) => op.specialized.get_or_init(|| program.specialize_columns(&columns).ok()).as_ref().unwrap_or(program),
            (None, false) => program,
        };
        let scopes = Bound::blank(rows);
        let mut outs = Outs::take();
        let mut failures = std::mem::take(&mut self.failures);
        failures.clear();
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
                (true, _) => match Self::templated(&array, local) {
                    Some(leaf) => leaf,
                    None => array.broadcast(local).map_or_else(|| Leaf::nulls(local), Leaf::typed),
                },
                (false, Some((_, ids))) => Leaf::typed(array.expand(ids)),
                (false, None) => Leaf::typed(array),
            };
            let leaf = self.store.place(leaf, positions);
            self.store.slots[*slot as usize] = Some(leaf);
        }
        if let (Some(entries), true, Some(step)) = (entries, report, self.graph.step_at(op.node)) {
            self.report(entries, step, positions, &failures);
        }
        self.failures = failures;
    }

    fn report(&mut self, entries: &[(Arc<str>, Arc<str>)], step: &crate::compiled::Step, positions: Option<&Rc<[usize]>>, failures: &[(usize, usize)]) {
        let mut messages: Vec<Option<Rc<str>>> = vec![None; entries.len().max(1)];
        for &(at, stage) in failures {
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
                        Some(positions) => leaf.regroup(positions),
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
                leaves = leaves.into_iter().map(|leaf| leaf.map(|leaf| leaf.regroup(&reps))).collect();
                reps.len()
            }
            None => local,
        };
        let mut columns = Columns::new(evaluated);
        columns.columns.reserve_exact(op.reads.len());
        let mut names: Vec<(Arc<str>, Bind)> = Vec::with_capacity(op.reads.len());
        let held: Vec<Option<Rc<Records<'a>>>> = match Records::LANE_VIEW {
            true => leaves.iter().map(|leaf| leaf.as_ref().and_then(Leaf::records)).collect(),
            false => Vec::new(),
        };
        let parts: Vec<Option<Vec<(&str, Column)>>> = match held.iter().any(Option::is_some) {
            true => held.iter().map(|r| r.as_ref().map(|r| r.parts())).collect(),
            false => Vec::new(),
        };
        let items: Vec<Option<Column>> = held.iter().zip(&parts).map(|(r, p)| Some(r.as_ref()?.item(p.as_ref()?))).collect();
        let views: Vec<Option<Column>> = leaves
            .iter()
            .enumerate()
            .map(|(at, leaf)| match (held.get(at).and_then(Option::as_ref), items.get(at).and_then(Option::as_ref)) {
                (Some(record), Some(item)) => Some(record.list(item)),
                _ => leaf.as_ref().map(Leaf::column),
            })
            .collect();
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
            TableMode::First => table.columns(table.first(&mut self.runner, &bound)),
            TableMode::Collected => {
                let collected = table.first_collect(&mut self.runner, &bound);
                (collected.columns, collected.present, collected.unmatched)
            }
            TableMode::Rows => {
                if let Some((records, codes, size)) = table.collect_records(&mut self.runner, &bound) {
                    let codes: Rc<[i32]> = match positions {
                        None => codes.into(),
                        Some(positions) => {
                            let mut full = vec![-1i32; self.store.rows];
                            positions.iter().zip(&codes).for_each(|(&row, &code)| full[row] = code);
                            full.into()
                        }
                    };
                    let leaf = match positions {
                        None => Leaf::Records(Rc::new(records)),
                        Some(positions) => match records.spread(positions, self.store.rows) {
                            Some(spread) => Leaf::Records(Rc::new(spread)),
                            None => self.store.place(Leaf::Records(Rc::new(records)), Some(positions)),
                        },
                    };
                    if let Some(slot) = op.outs.first() {
                        self.store.slots[*slot as usize] = Some(leaf);
                        self.store.hints.push((*slot, codes, size));
                    }
                    return;
                }
                let (values, hint) = match table.collect_keyed(&mut self.runner, &bound) {
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
                        table.evaluate(&mut self.runner, &bound).into_iter().map(|out| table.value(out)).collect::<Vec<Variable>>(),
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

    fn templated(array: &Array, rows: usize) -> Option<Leaf<'a>> {
        if !array.valid(0) {
            return None;
        }
        let Buffer::Any(values) = array.store() else {
            return None;
        };
        let literal = match values.first()? {
            Variable::Array(items) if items.borrow().iter().all(|item| matches!(item, Variable::Null | Variable::Bool(_) | Variable::Number(_) | Variable::String(_))) => return None,
            value @ (Variable::Array(_) | Variable::Object(_)) => Literal::of(value)?,
            _ => return None,
        };
        Some(Templated::leaf(vec![0i32; rows].into(), Rc::from(vec![literal])))
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
            typed: store.counts.0,
            laned: store.counts.1,
        }
    }
}

impl Plan {
    #[allow(clippy::too_many_arguments)]
    pub async fn evaluate<'a>(
        &self,
        graph: &CompiledGraph,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        columns: &'a Columns<'a>,
        failures: Vec<(usize, Rc<EvaluationError>)>,
    ) -> Option<ColumnarOutput<'a>> {
        if let Some(resolved) = &content.resolved_schemas {
            let touched = graph
                .steps
                .iter()
                .flatten()
                .any(|step| !matches!(step.kind, Kind::Input | Kind::Output) && resolved.contains_key(&step.node.id));
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
        let mut run = Run {
            graph,
            layout: &layout,
            runner: CompiledGraph::RUNNER.with_borrow_mut(std::mem::take),
            host: Host {
                content,
                extensions,
                max_depth,
            },
            store,
            settled: true,
            failures: Vec::new(),
            shreds: Vec::new(),
        };
        let done = run.walk(root.clone()).await;
        let Run { runner, store, .. } = run;
        CompiledGraph::RUNNER.with_borrow_mut(|slot| *slot = runner);
        done.ok()?;
        Some(Run::assemble(store))
    }
}

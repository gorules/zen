use crate::compiled::graph::plan::view::{Expr, MaskId, M, Node, Ref, SlotId};
use zen_expression::Variable;
use zen_types::symbol::Symbol;
use zen_types::variable::VariableMap;
use crate::compiled::graph::plan::{SegPlan, Source};
use zen_expression::typed::Outputs;
use crate::nodes::NodeError;
use crate::compiled::shred::{Listing, Normalizer};
use crate::compiled::core::exec::{Dedup, Hint};
use crate::compiled::leaf::{Bits, Dict, Leaf, Records};
use crate::EvaluationError;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{Column, Columns, Values};


struct Failure(Rc<str>);

pub(super) type Shredded<'a> = Option<(Rc<Records<'a>>, Rc<[u64]>)>;

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

pub(super) struct Shaped<'a> {
    leaf: Option<(Leaf<'a>, Rc<[u64]>)>,
    obj: Option<Rc<[u64]>>,
    fields: Vec<(Symbol, Shaped<'a>)>,
    same: Vec<Option<usize>>,
}

pub(super) type Entries<'g> = Option<&'g [(Arc<str>, Arc<str>)]>;
pub(super) type Spread = (Rc<[u64]>, Rc<[u64]>);
pub(super) type View<'v> = (Column<'v>, bool, Option<&'v Rc<[u64]>>, Option<&'v Rc<Dict>>);
pub(super) type Origin<'s, 'a> = Option<(&'s Leaf<'a>, &'s Option<Rc<[u64]>>)>;

type Group<'a> = (Option<Rc<[usize]>>, Vec<(Arc<str>, Leaf<'a>, Option<Rc<[u64]>>)>);
pub(super) type Lowered = (Outputs, Option<Vec<u32>>, Option<Vec<u64>>, Option<Vec<u64>>);
type Shown<'a> = (Leaf<'a>, Option<Rc<[u64]>>);
type Cached<'a> = (SlotId, Rc<str>, Option<Rc<Listing<'a>>>);
pub(super) type Prepared<'a> = (Rc<Listing<'a>>, Option<Vec<u64>>, Option<Vec<u64>>);
pub(super) type Branch = (Arc<SegPlan>, Option<Rc<[usize]>>);
pub(super) type Pending = (Arc<SegPlan>, Option<Rc<[usize]>>, Option<(usize, usize)>);
pub(super) type Flat<'a> = (Arc<str>, Leaf<'a>, Option<Rc<[u64]>>);
pub(super) type Layered<'a> = (Arc<str>, Leaf<'a>, Rc<[u64]>);

pub(crate) struct Store<'a> {
    pub(super) rows: usize,
    pub(super) words: usize,
    input: &'a Columns<'a>,
    /// The input already prepared (dates converted): read as it is.
    prepared: bool,
    pub(super) slots: Vec<Option<Leaf<'a>>>,
    inputs: Vec<Option<(Column<'a>, std::cell::OnceCell<Leaf<'a>>)>>,
    arena: Vec<u64>,
    offsets: Vec<u32>,
    masked: Vec<(SlotId, M, Leaf<'a>)>,
    shared: Vec<(usize, Rc<[u64]>)>,
    pub(super) ones: Rc<[u64]>,
    pub(super) errors: Vec<Option<Rc<EvaluationError>>>,
    recent: Vec<(Arc<str>, Rc<str>, Rc<EvaluationError>)>,
    pub(super) failed: usize,
    pub(super) groups: Vec<Group<'a>>,
    pub(super) hints: Vec<(SlotId, Rc<[i32]>, usize)>,
    views: Vec<(SlotId, bool, Option<Shown<'a>>)>,
    codes: Vec<(SlotId, bool, Hint)>,
    viewed: Vec<(SlotId, M, bool, Leaf<'a>)>,
    listings: std::cell::RefCell<Vec<Cached<'a>>>,
    pub(super) counts: (usize, usize),
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

    pub(super) fn new(seg: &SegPlan, input: &'a Columns<'a>, prepared: bool) -> Self {
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
            prepared,
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
            codes: Vec::new(),
            viewed: Vec::new(),
            listings: std::cell::RefCell::new(Vec::new()),
            counts: (0, 0),
        }
    }

    pub(super) fn enter(&mut self, seg: &SegPlan) {
        self.slots.resize(seg.slots.len(), None);
        self.offsets.resize(seg.masks.exprs.len(), Self::NONE);
    }

    pub(super) fn rewind(&mut self, (slots, masks): (usize, usize)) {
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
        self.codes.retain(|(slot, ..)| kept(*slot, M::All));
        self.viewed.retain(|(slot, m, ..)| kept(*slot, *m));
        self.listings.get_mut().retain(|(slot, ..)| kept(*slot, M::All));
    }

    pub(super) fn listing(&self, slot: SlotId, chain: &[&str]) -> Option<Rc<Listing<'a>>> {
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

    pub(super) fn leaf(&self, slot: SlotId) -> Leaf<'a> {
        if let Some(Some(leaf)) = self.slots.get(slot as usize) {
            return leaf.clone();
        }
        match self.inputs.get(slot as usize) {
            Some(Some((column, leaf))) => leaf
                .get_or_init(|| match self.prepared {
                    true => Normalizer::prepared(*column, self.rows),
                    false => Normalizer::input(*column, self.rows),
                })
                .clone(),
            _ => Leaf::nulls(self.rows),
        }
    }

    fn bits(&self, at: usize) -> &[u64] {
        &self.arena[at..at + self.words]
    }

    pub(super) fn mask(&mut self, seg: &SegPlan, m: M) -> Rc<[u64]> {
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

    pub(super) fn produce(&mut self, m: M, bits: Rc<[u64]>) {
        if let M::Id(id) = m {
            let at = self.push(&bits);
            if let Some(slot) = self.offsets.get_mut(id as usize) {
                *slot = at as u32;
            }
        }
    }

    pub(super) fn implied(seg: &SegPlan, r: Ref) -> bool {
        match r.present {
            M::All => true,
            M::None => false,
            M::Id(id) => matches!(seg.masks.exprs[id as usize], Expr::Validity(s) | Expr::Valued(s) if s == r.slot),
        }
    }

    pub(super) fn value(&mut self, seg: &SegPlan, r: Ref) -> Leaf<'a> {
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

    pub(super) fn codes(&mut self, slot: SlotId, code: bool, leaf: &Leaf<'a>) -> Hint {
        if let Some((.., hint)) = self.codes.iter().find(|(s, c, _)| *s == slot && *c == code) {
            return hint.clone();
        }
        let hint = Dedup::codes(&leaf.column(), self.rows);
        self.codes.push((slot, code, hint.clone()));
        hint
    }

    pub(super) fn viewed(&mut self, seg: &SegPlan, r: Ref, code: bool) -> (Leaf<'a>, Option<Rc<[u64]>>) {
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

    pub(super) fn fail(&mut self, row: usize, node: &crate::model::DecisionNode, message: Rc<str>) {
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

    pub(super) fn share(&mut self, row: usize, error: &Rc<EvaluationError>) {
        if let Some(slot @ None) = self.errors.get_mut(row) {
            self.failed += 1;
            *slot = Some(error.clone());
        }
    }

    pub(super) fn fault(&mut self, row: usize, error: NodeError) {
        if let Some(slot @ None) = self.errors.get_mut(row) {
            self.failed += 1;
            *slot = Some(Rc::new(EvaluationError::NodeError {
                node_id: error.node_id,
                source: error.source,
                trace: None,
            }));
        }
    }

    pub(super) fn shape(&mut self, seg: &SegPlan, node: &Node) -> Shaped<'a> {
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

    pub(super) fn objects(shape: &Shaped<'a>, rows: usize) -> Vec<Variable> {
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

    pub(super) fn merged(a: &Shaped<'a>, b: &Shaped<'a>, rr: bool, rb: bool, ra: bool, row: usize) -> Option<Variable> {
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

    pub(super) fn spread(&self, local: &Rc<[u64]>, positions: Option<&Rc<[usize]>>) -> Rc<[u64]> {
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

    pub(super) fn place(&self, leaf: Leaf<'a>, positions: Option<&Rc<[usize]>>) -> Leaf<'a> {
        match positions {
            None => leaf,
            Some(positions) => Leaf::scattered(leaf, positions.clone(), self.rows),
        }
    }
}

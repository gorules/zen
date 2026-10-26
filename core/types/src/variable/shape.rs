use crate::symbol::Symbol;
use ahash::HashMap;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

const MANY_AT: usize = 8;

const MAX_TRANSITIONS: usize = 4096;

const SWEEP_FLOOR: usize = 32;

const MERGE_CACHE: usize = 8;

pub(crate) struct MergePlan {
    pub(crate) slots: Rc<[u32]>,
    pub(crate) shape: Option<Rc<Shape>>,
}

#[derive(Clone)]
struct CachedPlan {
    slots: Rc<[u32]>,
    shape: Option<Weak<Shape>>,
}

type Cached = (u64, Option<CachedPlan>);

#[derive(Default)]
enum Transitions {
    #[default]
    Empty,
    Few(SmallVec<[(u64, Weak<Shape>); 4]>),
    Many {
        len: usize,
        sweep_at: usize,
        map: HashMap<u64, SmallVec<[Weak<Shape>; 1]>>,
    },
}

impl Transitions {
    fn len(&self) -> usize {
        match self {
            Transitions::Empty => 0,
            Transitions::Few(few) => few.len(),
            Transitions::Many { len, .. } => *len,
        }
    }

    fn find(&self, print: u64, matches: impl Fn(&Rc<Shape>) -> bool) -> Option<Rc<Shape>> {
        match self {
            Transitions::Empty => None,
            Transitions::Few(few) => few
                .iter()
                .filter(|(known, _)| *known == print)
                .filter_map(|(_, child)| child.upgrade())
                .find(|child| matches(child)),
            Transitions::Many { map, .. } => map
                .get(&print)?
                .iter()
                .filter_map(Weak::upgrade)
                .find(|child| matches(child)),
        }
    }

    fn sweep(&mut self) {
        match self {
            Transitions::Empty => {}
            Transitions::Few(few) => few.retain(|(_, child)| child.strong_count() > 0),
            Transitions::Many { len, sweep_at, map } => {
                map.retain(|_, children| {
                    children.retain(|child| child.strong_count() > 0);
                    !children.is_empty()
                });
                *len = map.values().map(SmallVec::len).sum();
                *sweep_at = (*len * 2).max(SWEEP_FLOOR);
            }
        }
    }

    fn full(&mut self) -> bool {
        if self.len() < MAX_TRANSITIONS {
            return false;
        }
        self.sweep();
        self.len() >= MAX_TRANSITIONS
    }

    fn push(&mut self, print: u64, child: &Rc<Shape>) {
        let child = Rc::downgrade(child);
        match self {
            Transitions::Empty => {
                let mut few = SmallVec::new();
                few.push((print, child));
                *self = Transitions::Few(few);
            }
            Transitions::Few(few) => {
                if let Some(dead) = few
                    .iter_mut()
                    .find(|(known, shape)| *known == print && shape.strong_count() == 0)
                {
                    dead.1 = child;
                    return;
                }
                if few.len() >= MANY_AT {
                    few.retain(|(_, known)| known.strong_count() > 0);
                }
                if few.len() < MANY_AT {
                    few.push((print, child));
                    return;
                }
                let mut map: HashMap<u64, SmallVec<[Weak<Shape>; 1]>> = HashMap::default();
                for (known, shape) in few.drain(..) {
                    map.entry(known).or_default().push(shape);
                }
                map.entry(print).or_default().push(child);
                let len = MANY_AT + 1;
                *self = Transitions::Many {
                    len,
                    sweep_at: (len * 2).max(SWEEP_FLOOR),
                    map,
                };
            }
            Transitions::Many { len, sweep_at, map } => {
                let children = map.entry(print).or_default();
                if let Some(dead) = children.iter_mut().find(|shape| shape.strong_count() == 0) {
                    *dead = child;
                    return;
                }
                children.push(child);
                *len += 1;
                if *len >= *sweep_at {
                    self.sweep();
                }
            }
        }
    }
}

thread_local! {
    static ROOT: Rc<Shape> = Rc::new(Shape::new(Box::new([]), None));
}

pub struct Shape {
    id: u64,
    keys: Box<[Symbol]>,
    prints: Box<[u64]>,
    parent: Option<Rc<Shape>>,
    transitions: RefCell<Transitions>,
    merges: RefCell<SmallVec<[Cached; 2]>>,
}

impl Shape {
    fn new(keys: Box<[Symbol]>, parent: Option<Rc<Shape>>) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            parent,
            prints: keys.iter().map(|key| Self::print(key.as_str())).collect(),
            keys,
            transitions: RefCell::new(Transitions::default()),
            merges: RefCell::new(SmallVec::new()),
        }
    }

    #[inline]
    fn print(key: &str) -> u64 {
        let bytes = key.as_bytes();
        let mut packed = [0u8; 8];
        let take = bytes.len().min(7);
        packed[..take].copy_from_slice(&bytes[..take]);
        packed[7] = bytes.len().min(255) as u8;
        u64::from_le_bytes(packed)
    }

    #[inline]
    fn exact(key: &str) -> bool {
        key.len() <= 7
    }

    pub fn root() -> Rc<Shape> {
        ROOT.with(Rc::clone)
    }

    pub fn of<'a>(keys: impl IntoIterator<Item = &'a Symbol>) -> Option<Rc<Shape>> {
        keys.into_iter()
            .try_fold(Self::root(), |shape, key| shape.with(key))
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn keys(&self) -> &[Symbol] {
        &self.keys
    }

    pub fn parent(&self) -> Option<&Rc<Shape>> {
        self.parent.as_ref()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    #[inline]
    pub fn index_of(&self, key: &str) -> Option<usize> {
        let print = Self::print(key);
        if Self::exact(key) {
            return self.prints.iter().position(|known| *known == print);
        }
        self.prints
            .iter()
            .enumerate()
            .filter(|(_, known)| **known == print)
            .map(|(index, _)| index)
            .find(|&index| self.keys[index].as_str() == key)
    }

    #[inline]
    pub fn key_at(&self, index: usize) -> Option<&Symbol> {
        self.keys.get(index)
    }

    #[inline]
    pub fn transition(&self, key: &str) -> Option<Rc<Shape>> {
        let position = self.keys.len();
        let exact = Self::exact(key);
        self.transitions.borrow().find(Self::print(key), |child| {
            exact
                || child
                    .key_at(position)
                    .is_some_and(|known| known.as_str() == key)
        })
    }

    pub fn with(self: &Rc<Self>, key: &Symbol) -> Option<Rc<Shape>> {
        if let Some(child) = self.transition(key.as_str()) {
            return Some(child);
        }
        let mut transitions = self.transitions.borrow_mut();
        if transitions.full() {
            return None;
        }
        let mut keys = Vec::with_capacity(self.keys.len() + 1);
        keys.extend(self.keys.iter().cloned());
        keys.push(key.clone());
        let child = Rc::new(Shape::new(keys.into_boxed_slice(), Some(self.clone())));
        transitions.push(Self::print(key.as_str()), &child);
        Some(child)
    }

    pub(crate) fn merge_plan(self: &Rc<Self>, patch: &Shape, spill_at: usize) -> Option<MergePlan> {
        let id = patch.id;
        let cached = self
            .merges
            .borrow()
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, plan)| plan.clone());
        match cached {
            Some(None) => return None,
            Some(Some(CachedPlan { slots, shape: None })) => {
                return Some(MergePlan { slots, shape: None });
            }
            Some(Some(CachedPlan {
                slots,
                shape: Some(weak),
            })) => {
                if let Some(shape) = weak.upgrade() {
                    return Some(MergePlan {
                        slots,
                        shape: Some(shape),
                    });
                }
            }
            None => {}
        }
        let plan = self.plan_merge(patch, spill_at);
        let entry = plan.as_ref().map(|plan| CachedPlan {
            slots: plan.slots.clone(),
            shape: plan.shape.as_ref().map(Rc::downgrade),
        });
        let mut merges = self.merges.borrow_mut();
        match merges.iter().position(|(known, _)| *known == id) {
            Some(index) => merges[index] = (id, entry),
            None if merges.len() < MERGE_CACHE => merges.push((id, entry)),
            None => merges[(id as usize) % MERGE_CACHE] = (id, entry),
        }
        plan
    }
    fn plan_merge(self: &Rc<Self>, patch: &Shape, spill_at: usize) -> Option<MergePlan> {
        let base = self.len();
        let mut current = self.clone();
        let mut slots = Vec::with_capacity(patch.len());
        for key in patch.keys.iter() {
            match current.index_of(key.as_str()) {
                Some(index) if index < base => slots.push(index as u32),
                Some(_) => return None,
                None if current.len() >= spill_at => return None,
                None => {
                    let child = current.with(key)?;
                    slots.push(current.len() as u32);
                    current = child;
                }
            }
        }
        Some(MergePlan {
            slots: slots.into(),
            shape: (!Rc::ptr_eq(&current, self)).then_some(current),
        })
    }

    pub fn without(&self, index: usize) -> Option<Rc<Shape>> {
        Self::of(
            self.keys
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index)
                .map(|(_, key)| key),
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct ShapeHint {
    shapes: [u64; 2],
    indices: [u32; 2],
    next: u8,
}

impl ShapeHint {
    pub const ABSENT: u32 = u32::MAX;

    #[inline]
    pub fn lookup(&self, shape: u64) -> Option<u32> {
        if self.shapes[0] == shape {
            return Some(self.indices[0]);
        }
        if self.shapes[1] == shape {
            return Some(self.indices[1]);
        }
        None
    }

    #[inline]
    pub fn record(&mut self, shape: u64, index: Option<usize>) {
        let slot = self.next as usize;
        self.shapes[slot] = shape;
        self.indices[slot] = index.map_or(Self::ABSENT, |index| index as u32);
        self.next ^= 1;
    }
}

impl Debug for Shape {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shape")
            .field("id", &self.id)
            .field("keys", &self.keys)
            .finish()
    }
}

impl PartialEq for Shape {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Shape {}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> Symbol {
        Symbol::from(text)
    }

    fn chain(keys: &[&str]) -> Rc<Shape> {
        let keys: Vec<Symbol> = keys.iter().map(|k| key(k)).collect();
        Shape::of(keys.iter()).expect("unsaturated")
    }

    #[test]
    fn transitions_are_interned_per_key_sequence() {
        let a = chain(&["id", "balance"]);
        let b = chain(&["id", "balance"]);
        let c = chain(&["balance", "id"]);
        assert!(Rc::ptr_eq(&a, &b));
        assert_eq!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
        assert_eq!(a.index_of("balance"), Some(1));
        assert_eq!(c.index_of("balance"), Some(0));
        assert_eq!(a.index_of("missing"), None);
    }

    #[test]
    fn long_keys_with_a_shared_prefix_stay_distinct() {
        let shape = chain(&["interestDue", "interestRate", "id"]);
        assert_eq!(shape.index_of("interestDue"), Some(0));
        assert_eq!(shape.index_of("interestRate"), Some(1));
        assert_eq!(shape.index_of("interestDuo"), None);
        assert_eq!(shape.index_of("id"), Some(2));
        assert_eq!(shape.index_of("idx"), None);
        let a = chain(&["interestDue"]);
        let b = chain(&["interestRate"]);
        assert!(!Rc::ptr_eq(&a, &b));
        assert!(Rc::ptr_eq(&chain(&["interestRate"]), &b));
    }

    #[test]
    fn removing_a_key_reuses_the_interned_shape() {
        let shape = chain(&["a", "b", "c"]);
        let without = shape.without(1).expect("unsaturated");
        assert!(Rc::ptr_eq(&without, &chain(&["a", "c"])));
    }

    #[test]
    fn wide_fan_out_stays_interned_until_saturation() {
        let root = chain(&["fanout-fixed"]);
        let children: Vec<Rc<Shape>> = (0..MAX_TRANSITIONS)
            .map(|i| root.with(&key(&format!("k{i}"))).expect("below the cap"))
            .collect();
        assert_eq!(root.transitions.borrow().len(), MAX_TRANSITIONS);
        for (i, child) in children.iter().enumerate() {
            assert_eq!(child.index_of(&format!("k{i}")), Some(1));
            assert!(Rc::ptr_eq(
                &root.with(&key(&format!("k{i}"))).expect("interned"),
                child
            ));
        }
        assert!(root.with(&key("one-too-many")).is_none());
    }

    fn root_transitions() -> usize {
        Shape::root().transitions.borrow().len()
    }

    #[test]
    fn a_shape_is_freed_once_nothing_uses_it() {
        let leaf = chain(&["freed-a", "freed-b", "freed-c"]);
        let middle = Rc::downgrade(leaf.parent().unwrap());
        let weak = Rc::downgrade(&leaf);
        drop(leaf);
        assert!(weak.upgrade().is_none());
        assert!(middle.upgrade().is_none());
        assert!(Shape::root().transition("freed-a").is_none());
    }

    #[test]
    fn a_live_shape_keeps_its_prefixes_interned() {
        let leaf = chain(&["kept-a", "kept-b", "kept-c"]);
        let again = chain(&["kept-a", "kept-b", "kept-c"]);
        assert!(Rc::ptr_eq(&leaf, &again));
        assert!(Rc::ptr_eq(
            leaf.parent().unwrap(),
            &chain(&["kept-a", "kept-b"])
        ));
        let id = leaf.id();
        drop((leaf, again));
        assert_ne!(chain(&["kept-a", "kept-b", "kept-c"]).id(), id);
    }

    #[test]
    fn dead_transitions_are_swept_under_unbounded_key_layouts() {
        let before = root_transitions();
        let first = Rc::downgrade(&chain(&["dyn-first", "id"]));
        for i in 0..(MAX_TRANSITIONS * 4) {
            let shape = chain(&[&format!("dyn{i}"), "id"]);
            assert_eq!(shape.index_of("id"), Some(1));
        }
        assert!(root_transitions() <= (before + 1) * 2 + SWEEP_FLOOR);
        assert!(first.upgrade().is_none());
    }

    #[test]
    fn recreating_a_freed_key_reuses_its_dead_link() {
        let base = chain(&["reuse-base"]);
        let wide = chain(&["reuse-wide"]);
        let alive: Vec<Rc<Shape>> = (0..(MANY_AT * 2))
            .map(|i| wide.with(&key(&format!("w{i}"))).expect("unsaturated"))
            .collect();
        for _ in 0..1000 {
            let child = base.with(&key("reused")).expect("unsaturated");
            assert_eq!(child.index_of("reused"), Some(1));
            let child = wide.with(&key("reused")).expect("unsaturated");
            assert_eq!(child.index_of("reused"), Some(1));
        }
        assert_eq!(base.transitions.borrow().len(), 1);
        assert_eq!(wide.transitions.borrow().len(), MANY_AT * 2 + 1);
        let Transitions::Many { map, .. } = &*wide.transitions.borrow() else {
            panic!("wide shape should use the map");
        };
        assert_eq!(map.get(&Shape::print("reused")).map(SmallVec::len), Some(1));
        assert_eq!(alive.len(), MANY_AT * 2);
    }

    #[test]
    fn a_cached_merge_does_not_keep_shapes_alive() {
        let base = chain(&["merge-a"]);
        let patch = chain(&["merge-b"]);
        let plan = base.merge_plan(&patch, 32).expect("plan");
        let merged = Rc::downgrade(plan.shape.as_ref().unwrap());
        assert_eq!(&*plan.slots, &[1]);
        drop(plan);
        assert!(merged.upgrade().is_none());
        let again = base.merge_plan(&patch, 32).expect("plan");
        assert_eq!(again.shape.as_ref().unwrap().keys().len(), 2);
        let weak_base = Rc::downgrade(&base);
        drop((base, again));
        assert!(weak_base.upgrade().is_none());
    }
}

use crate::symbol::Symbol;
use ahash::HashMap;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

const MANY_AT: usize = 8;

const MAX_TRANSITIONS: usize = 4096;

const MERGE_CACHE: usize = 8;

type Cached = (u64, Option<Rc<MergePlan>>);

pub(crate) struct MergePlan {
    pub(crate) slots: Box<[u32]>,
    pub(crate) shape: Option<Rc<Shape>>,
}

#[derive(Default)]
enum Transitions {
    #[default]
    Empty,
    Few(SmallVec<[(u64, Rc<Shape>); 4]>),
    Many(usize, HashMap<u64, SmallVec<[Rc<Shape>; 1]>>),
}

impl Transitions {
    fn len(&self) -> usize {
        match self {
            Transitions::Empty => 0,
            Transitions::Few(few) => few.len(),
            Transitions::Many(len, _) => *len,
        }
    }

    fn push(&mut self, print: u64, child: Rc<Shape>) {
        match self {
            Transitions::Empty => {
                let mut few = SmallVec::new();
                few.push((print, child));
                *self = Transitions::Few(few);
            }
            Transitions::Few(few) if few.len() < MANY_AT => few.push((print, child)),
            Transitions::Few(few) => {
                let mut map: HashMap<u64, SmallVec<[Rc<Shape>; 1]>> = HashMap::default();
                for (known, shape) in few.drain(..) {
                    map.entry(known).or_default().push(shape);
                }
                map.entry(print).or_default().push(child);
                *self = Transitions::Many(MANY_AT + 1, map);
            }
            Transitions::Many(len, map) => {
                map.entry(print).or_default().push(child);
                *len += 1;
            }
        }
    }
}

thread_local! {
    static ROOT: Rc<Shape> = Rc::new(Shape::new(Box::new([])));
}

pub struct Shape {
    id: u64,
    keys: Box<[Symbol]>,
    prints: Box<[u64]>,
    transitions: RefCell<Transitions>,
    merges: RefCell<SmallVec<[Cached; 2]>>,
}

impl Shape {
    fn new(keys: Box<[Symbol]>) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
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
        let print = Self::print(key);
        let transitions = self.transitions.borrow();
        let position = self.keys.len();
        let exact = Self::exact(key);
        let matches = |child: &Rc<Shape>| {
            exact
                || child
                    .key_at(position)
                    .is_some_and(|known| known.as_str() == key)
        };
        match &*transitions {
            Transitions::Empty => None,
            Transitions::Few(few) => few
                .iter()
                .find(|(known, child)| *known == print && matches(child))
                .map(|(_, child)| child.clone()),
            Transitions::Many(_, map) => map.get(&print)?.iter().find(|child| matches(child)).cloned(),
        }
    }

    pub fn with(self: &Rc<Self>, key: &Symbol) -> Option<Rc<Shape>> {
        if let Some(child) = self.transition(key.as_str()) {
            return Some(child);
        }
        let mut transitions = self.transitions.borrow_mut();
        if transitions.len() >= MAX_TRANSITIONS {
            return None;
        }
        let mut keys = Vec::with_capacity(self.keys.len() + 1);
        keys.extend(self.keys.iter().cloned());
        keys.push(key.clone());
        let child = Rc::new(Shape::new(keys.into_boxed_slice()));
        transitions.push(Self::print(key.as_str()), child.clone());
        Some(child)
    }

    pub(crate) fn merge_plan(self: &Rc<Self>, patch: &Shape, spill_at: usize) -> Option<Rc<MergePlan>> {
        let id = patch.id;
        if let Some((_, plan)) = self.merges.borrow().iter().find(|(known, _)| *known == id) {
            return plan.clone();
        }
        let plan = self.plan_merge(patch, spill_at).map(Rc::new);
        let mut merges = self.merges.borrow_mut();
        match merges.len() < MERGE_CACHE {
            true => merges.push((id, plan.clone())),
            false => merges[(id as usize) % MERGE_CACHE] = (id, plan.clone()),
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
            slots: slots.into_boxed_slice(),
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
}

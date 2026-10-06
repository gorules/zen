use crate::symbol::Symbol;
use crate::variable::Variable;
use crate::variable::shape::{Shape, ShapeHint};
use ahash::{HashMap, HashMapExt};
use smallvec::SmallVec;
use std::fmt::{Debug, Formatter};
use std::rc::Rc;

const INLINE: usize = 8;

const SPILL_AT: usize = 32;

type Values = SmallVec<[Variable; INLINE]>;

#[derive(Clone)]
enum Repr {
    Shaped { shape: Rc<Shape>, values: Values },
    Dict(HashMap<Symbol, Variable>),
}

#[derive(Clone)]
pub struct VariableMap(Repr);

impl VariableMap {
    pub fn new() -> Self {
        Self(Repr::Shaped {
            shape: Shape::root(),
            values: SmallVec::new(),
        })
    }

    pub fn from_shape(shape: Rc<Shape>, values: impl IntoIterator<Item = Variable>) -> Self {
        Self(Repr::Shaped {
            shape,
            values: values.into_iter().collect(),
        })
    }

    pub fn with_capacity(capacity: usize) -> Self {
        match capacity > SPILL_AT {
            true => Self(Repr::Dict(HashMap::with_capacity(capacity))),
            false => Self(Repr::Shaped {
                shape: Shape::root(),
                values: SmallVec::with_capacity(capacity),
            }),
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        match &mut self.0 {
            Repr::Shaped { values, .. } => values.reserve(additional),
            Repr::Dict(map) => map.reserve(additional),
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Shaped { values, .. } => values.len(),
            Repr::Dict(map) => map.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        match &mut self.0 {
            Repr::Shaped { shape, values } => {
                *shape = Shape::root();
                values.clear();
            }
            Repr::Dict(map) => map.clear(),
        }
    }

    #[inline]
    pub fn shape(&self) -> Option<&Rc<Shape>> {
        match &self.0 {
            Repr::Shaped { shape, .. } => Some(shape),
            Repr::Dict(_) => None,
        }
    }

    #[inline]
    pub fn shape_id(&self) -> Option<u64> {
        self.shape().map(|shape| shape.id())
    }

    #[inline]
    pub fn get(&self, key: &Symbol) -> Option<&Variable> {
        self.get_str(key.as_str())
    }

    #[inline]
    pub fn get_str(&self, key: &str) -> Option<&Variable> {
        match &self.0 {
            Repr::Shaped { shape, values } => shape.index_of(key).map(|index| &values[index]),
            Repr::Dict(map) => map.get(key),
        }
    }

    #[inline]
    pub fn get_hinted(&self, hint: &mut ShapeHint, key: &str) -> Option<&Variable> {
        match &self.0 {
            Repr::Shaped { shape, values } => {
                let id = shape.id();
                if let Some(index) = hint.lookup(id) {
                    return values.get(index as usize);
                }
                let index = shape.index_of(key);
                hint.record(id, index);
                index.map(|index| &values[index])
            }
            Repr::Dict(map) => map.get(key),
        }
    }

    pub fn map_values(&self, mut f: impl FnMut(&Variable) -> Variable) -> Self {
        match &self.0 {
            Repr::Shaped { shape, values } => Self(Repr::Shaped {
                shape: shape.clone(),
                values: values.iter().map(&mut f).collect(),
            }),
            Repr::Dict(map) => Self(Repr::Dict(
                map.iter()
                    .map(|(key, value)| (key.clone(), f(value)))
                    .collect(),
            )),
        }
    }

    pub fn get_mut(&mut self, key: &Symbol) -> Option<&mut Variable> {
        self.get_mut_str(key.as_str())
    }

    pub fn get_mut_str(&mut self, key: &str) -> Option<&mut Variable> {
        match &mut self.0 {
            Repr::Shaped { shape, values } => shape.index_of(key).map(|index| &mut values[index]),
            Repr::Dict(map) => map.get_mut(key),
        }
    }

    pub fn get_key_value(&self, key: &Symbol) -> Option<(&Symbol, &Variable)> {
        match &self.0 {
            Repr::Shaped { shape, values } => shape
                .index_of(key.as_str())
                .and_then(|index| Some((shape.key_at(index)?, &values[index]))),
            Repr::Dict(map) => map.get_key_value(key),
        }
    }

    pub fn contains_key(&self, key: &Symbol) -> bool {
        self.get(key).is_some()
    }

    pub fn contains_key_str(&self, key: &str) -> bool {
        self.get_str(key).is_some()
    }

    pub fn remove_str(&mut self, key: &str) -> Option<Variable> {
        match &mut self.0 {
            Repr::Shaped { shape, values } => {
                let index = shape.index_of(key)?;
                match shape.without(index) {
                    Some(reduced) => {
                        *shape = reduced;
                        Some(values.remove(index))
                    }
                    None => {
                        self.spill();
                        self.remove_str(key)
                    }
                }
            }
            Repr::Dict(map) => map.remove(key),
        }
    }

    pub fn insert(&mut self, key: Symbol, value: Variable) -> Option<Variable> {
        match &mut self.0 {
            Repr::Shaped { shape, values } => {
                let transition = (values.len() < SPILL_AT)
                    .then(|| shape.transition(key.as_str()))
                    .flatten();
                if let Some(child) = transition {
                    *shape = child;
                    values.push(value);
                    return None;
                }
                if let Some(index) = shape.index_of(key.as_str()) {
                    return Some(std::mem::replace(&mut values[index], value));
                }
                if values.len() >= SPILL_AT {
                    self.spill();
                    let Repr::Dict(map) = &mut self.0 else {
                        unreachable!("just spilled")
                    };
                    return map.insert(key, value);
                }
                self.grow(key, value)
            }
            Repr::Dict(map) => map.insert(key, value),
        }
    }

    pub fn insert_new(&mut self, key: Symbol, value: Variable) {
        match &self.0 {
            Repr::Shaped { values, .. } if values.len() < SPILL_AT => {
                self.grow(key, value);
            }
            _ => {
                self.insert(key, value);
            }
        }
    }

    #[inline]
    fn follow(&mut self, key: &str, value: Variable) -> Option<Variable> {
        let Repr::Shaped { shape, values } = &mut self.0 else {
            return Some(value);
        };
        if values.len() >= SPILL_AT {
            return Some(value);
        }
        let Some(child) = shape.transition(key) else {
            return Some(value);
        };
        *shape = child;
        values.push(value);
        None
    }

    fn grow(&mut self, key: Symbol, value: Variable) -> Option<Variable> {
        let Repr::Shaped { shape, values } = &mut self.0 else {
            return self.insert(key, value);
        };
        match shape.with(&key) {
            Some(child) => {
                *shape = child;
                values.push(value);
                None
            }
            None => {
                self.spill();
                self.insert(key, value)
            }
        }
    }

    pub fn insert_new_str(&mut self, key: &str, value: Variable) {
        if let Some(value) = self.follow(key, value) {
            self.insert_new(Symbol::from(key), value);
        }
    }

    pub fn remove(&mut self, key: &Symbol) -> Option<Variable> {
        self.remove_str(key.as_str())
    }

    fn spill(&mut self) {
        let Repr::Shaped { shape, values } = &mut self.0 else {
            return;
        };
        let mut map = HashMap::with_capacity(values.len() * 2);
        for (key, value) in shape.keys().iter().zip(values.drain(..)) {
            map.insert(key.clone(), value);
        }
        self.0 = Repr::Dict(map);
    }

    pub fn entry(&mut self, key: Symbol) -> Entry<'_> {
        if matches!(&self.0, Repr::Shaped { values, .. }
            if values.len() >= SPILL_AT && self.get(&key).is_none())
        {
            self.spill();
        }

        match self.contains_key(&key) {
            true => Entry::Occupied(OccupiedEntry { map: self, key }),
            false => Entry::Vacant(VacantEntry { map: self, key }),
        }
    }

    pub fn iter(&self) -> Iter<'_> {
        match &self.0 {
            Repr::Shaped { shape, values } => Iter::Shaped(shape.keys().iter().zip(values.iter())),
            Repr::Dict(map) => Iter::Dict(map.iter()),
        }
    }

    pub fn iter_mut(&mut self) -> IterMut<'_> {
        match &mut self.0 {
            Repr::Shaped { shape, values } => {
                IterMut::Shaped(shape.keys().iter().zip(values.iter_mut()))
            }
            Repr::Dict(map) => IterMut::Dict(map.iter_mut()),
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &Symbol> + '_ {
        self.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl Iterator<Item = &Variable> {
        self.iter().map(|(_, value)| value)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Variable> {
        self.iter_mut().map(|(_, value)| value)
    }
}

pub enum Entry<'a> {
    Occupied(OccupiedEntry<'a>),
    Vacant(VacantEntry<'a>),
}

pub struct OccupiedEntry<'a> {
    map: &'a mut VariableMap,
    key: Symbol,
}

pub struct VacantEntry<'a> {
    map: &'a mut VariableMap,
    key: Symbol,
}

impl<'a> Entry<'a> {
    pub fn or_insert(self, default: Variable) -> &'a mut Variable {
        match self {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(default),
        }
    }

    pub fn or_insert_with<F: FnOnce() -> Variable>(self, default: F) -> &'a mut Variable {
        match self {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(default()),
        }
    }
}

impl<'a> OccupiedEntry<'a> {
    pub fn get(&self) -> &Variable {
        self.map.get(&self.key).expect("occupied")
    }

    pub fn get_mut(&mut self) -> &mut Variable {
        self.map.get_mut(&self.key).expect("occupied")
    }

    pub fn into_mut(self) -> &'a mut Variable {
        let key = self.key;
        self.map.get_mut(&key).expect("occupied")
    }

    pub fn insert(&mut self, value: Variable) -> Variable {
        std::mem::replace(self.get_mut(), value)
    }
}

impl<'a> VacantEntry<'a> {
    pub fn insert(self, value: Variable) -> &'a mut Variable {
        let key = self.key;
        self.map.insert(key.clone(), value);
        self.map.get_mut(&key).expect("just inserted")
    }
}

pub enum Iter<'a> {
    Shaped(std::iter::Zip<std::slice::Iter<'a, Symbol>, std::slice::Iter<'a, Variable>>),
    Dict(std::collections::hash_map::Iter<'a, Symbol, Variable>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Symbol, &'a Variable);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Iter::Shaped(iter) => iter.next(),
            Iter::Dict(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Iter::Shaped(iter) => iter.size_hint(),
            Iter::Dict(iter) => iter.size_hint(),
        }
    }
}

pub enum IterMut<'a> {
    Shaped(std::iter::Zip<std::slice::Iter<'a, Symbol>, std::slice::IterMut<'a, Variable>>),
    Dict(std::collections::hash_map::IterMut<'a, Symbol, Variable>),
}

impl<'a> Iterator for IterMut<'a> {
    type Item = (&'a Symbol, &'a mut Variable);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            IterMut::Shaped(iter) => iter.next(),
            IterMut::Dict(iter) => iter.next(),
        }
    }
}

pub enum IntoIter {
    Shaped {
        shape: Rc<Shape>,
        index: usize,
        values: smallvec::IntoIter<[Variable; INLINE]>,
    },
    Dict(std::collections::hash_map::IntoIter<Symbol, Variable>),
}

impl Iterator for IntoIter {
    type Item = (Symbol, Variable);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            IntoIter::Shaped {
                shape,
                index,
                values,
            } => {
                let value = values.next()?;
                let key = shape.key_at(*index)?.clone();
                *index += 1;
                Some((key, value))
            }
            IntoIter::Dict(iter) => iter.next(),
        }
    }
}

impl IntoIterator for VariableMap {
    type Item = (Symbol, Variable);
    type IntoIter = IntoIter;

    fn into_iter(self) -> IntoIter {
        match self.0 {
            Repr::Shaped { shape, values } => IntoIter::Shaped {
                shape,
                index: 0,
                values: values.into_iter(),
            },
            Repr::Dict(map) => IntoIter::Dict(map.into_iter()),
        }
    }
}

impl<'a> IntoIterator for &'a VariableMap {
    type Item = (&'a Symbol, &'a Variable);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl Default for VariableMap {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<(Symbol, Variable)> for VariableMap {
    fn from_iter<T: IntoIterator<Item = (Symbol, Variable)>>(iter: T) -> Self {
        let iter = iter.into_iter();
        let mut map = VariableMap::with_capacity(iter.size_hint().0);
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl Extend<(Symbol, Variable)> for VariableMap {
    fn extend<T: IntoIterator<Item = (Symbol, Variable)>>(&mut self, iter: T) {
        for (key, value) in iter {
            self.insert(key, value);
        }
    }
}

impl PartialEq for VariableMap {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(key, value)| other.get(key).is_some_and(|o| o == value))
    }
}

impl VariableMap {
    pub fn insert_str(&mut self, key: &str, value: Variable) -> Option<Variable> {
        let value = self.follow(key, value)?;
        self.insert(Symbol::from(key), value)
    }
}

impl Debug for VariableMap {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.iter().map(|(k, v)| (k.as_str(), v)))
            .finish()
    }
}

impl serde::Serialize for VariableMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key.as_str(), value)?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(keys: &[&str]) -> VariableMap {
        let mut map = VariableMap::new();
        for (index, key) in keys.iter().enumerate() {
            map.insert(Symbol::from(*key), Variable::Number(index.into()));
        }
        map
    }

    #[test]
    fn maps_built_from_the_same_keys_share_a_shape() {
        let a = map(&["id", "balance", "apr"]);
        let b = map(&["id", "balance", "apr"]);
        let c = map(&["balance", "id", "apr"]);
        assert_eq!(a.shape_id(), b.shape_id());
        assert_ne!(a.shape_id(), c.shape_id());
        assert_eq!(a.get_str("balance"), Some(&Variable::Number(1.into())));
        assert_eq!(
            a.keys().map(Symbol::as_str).collect::<Vec<_>>(),
            ["id", "balance", "apr"]
        );
    }

    #[test]
    fn replacing_keeps_the_shape_and_removing_reshapes() {
        let mut a = map(&["id", "balance", "apr"]);
        let before = a.shape_id();
        assert_eq!(
            a.insert(Symbol::from("balance"), Variable::Bool(true)),
            Some(Variable::Number(1.into()))
        );
        assert_eq!(a.shape_id(), before);
        assert_eq!(a.remove_str("balance"), Some(Variable::Bool(true)));
        assert_eq!(a.shape_id(), map(&["id", "apr"]).shape_id());
        assert_eq!(a.get_str("apr"), Some(&Variable::Number(2.into())));
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn hinted_reads_cache_two_shapes_including_absence() {
        let a = map(&["id", "balance", "apr"]);
        let b = map(&["balance", "id"]);
        let mut hint = ShapeHint::default();
        assert_eq!(
            a.get_hinted(&mut hint, "balance"),
            Some(&Variable::Number(1.into()))
        );
        assert_eq!(
            b.get_hinted(&mut hint, "balance"),
            Some(&Variable::Number(0.into()))
        );
        assert_eq!(hint.lookup(a.shape_id().unwrap()), Some(1));
        assert_eq!(hint.lookup(b.shape_id().unwrap()), Some(0));
        let mut missing = ShapeHint::default();
        assert_eq!(a.get_hinted(&mut missing, "missing"), None);
        assert_eq!(
            missing.lookup(a.shape_id().unwrap()),
            Some(ShapeHint::ABSENT)
        );
        assert_eq!(a.get_hinted(&mut missing, "missing"), None);
    }

    #[test]
    fn map_values_keeps_the_shape() {
        let source = map(&["id", "balance"]);
        let mapped = source.map_values(|value| match value {
            Variable::Number(n) => Variable::Number(*n * rust_decimal::Decimal::TWO),
            other => other.shallow_clone(),
        });
        assert_eq!(mapped.shape_id(), source.shape_id());
        assert_eq!(mapped.get_str("balance"), Some(&Variable::Number(2.into())));
    }

    #[test]
    fn spilled_maps_have_no_shape_but_keep_their_contents() {
        let keys: Vec<String> = (0..40).map(|i| format!("k{i}")).collect();
        let mut map = VariableMap::new();
        for key in &keys {
            map.insert(Symbol::from(key.as_str()), Variable::Null);
        }
        assert!(map.shape().is_none());
        let mut hint = ShapeHint::default();
        assert_eq!(map.get_hinted(&mut hint, "k39"), Some(&Variable::Null));
        assert_eq!(
            map.insert(Symbol::from("k39"), Variable::Bool(false)),
            Some(Variable::Null)
        );
        assert_eq!(
            map.insert(Symbol::from("fresh"), Variable::Bool(true)),
            None
        );
        assert_eq!(map.len(), 41);
        let owned: Vec<(Symbol, Variable)> = map.into_iter().collect();
        assert_eq!(owned.len(), 41);
    }

    #[test]
    fn string_keyed_inserts_follow_transitions_or_fall_back() {
        let mut first = VariableMap::new();
        first.insert_new_str("approval", Variable::Bool(false));
        first.insert_new_str("rejectionReasons", Variable::Null);
        assert_eq!(first.len(), 2);
        assert_eq!(first.get_str("approval"), Some(&Variable::Bool(false)));
        let mut second = VariableMap::new();
        assert_eq!(second.insert_str("approval", Variable::Bool(true)), None);
        assert_eq!(second.insert_str("rejectionReasons", Variable::Null), None);
        assert_eq!(second.shape_id(), first.shape_id());
        assert_eq!(
            second.insert_str("approval", Variable::Null),
            Some(Variable::Bool(true))
        );
        assert_eq!(second.len(), 2);
        let mut spilled = VariableMap::new();
        for i in 0..40 {
            spilled.insert_new_str(&format!("key{i}"), Variable::Null);
        }
        assert_eq!(spilled.len(), 40);
        assert_eq!(
            spilled.insert_str("key39", Variable::Bool(true)),
            Some(Variable::Null)
        );
        assert_eq!(spilled.insert_str("fresh", Variable::Bool(true)), None);
        assert_eq!(spilled.len(), 41);
    }

    #[test]
    fn owned_iteration_preserves_order() {
        let map = map(&["z", "a", "m"]);
        let keys: Vec<String> = map
            .into_iter()
            .map(|(key, _)| key.as_str().to_owned())
            .collect();
        assert_eq!(keys, ["z", "a", "m"]);
    }
}

use crate::symbol::Symbol;
use crate::variable::Variable;
use ahash::{HashMap, HashMapExt};
use smallvec::SmallVec;
use std::fmt::{Debug, Formatter};

const INLINE: usize = 8;

const SPILL_AT: usize = 32;

type Entries = SmallVec<[(Symbol, Variable); INLINE]>;

#[derive(Clone)]
enum Repr {
    Small(Entries),
    Large(HashMap<Symbol, Variable>),
}

#[derive(Clone)]
pub struct VariableMap(Repr);

impl VariableMap {
    pub fn new() -> Self {
        Self(Repr::Small(SmallVec::new()))
    }

    pub fn with_capacity(capacity: usize) -> Self {
        match capacity > SPILL_AT {
            true => Self(Repr::Large(HashMap::with_capacity(capacity))),
            false => Self(Repr::Small(SmallVec::with_capacity(capacity))),
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Small(entries) => entries.len(),
            Repr::Large(map) => map.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        match &mut self.0 {
            Repr::Small(entries) => entries.clear(),
            Repr::Large(map) => map.clear(),
        }
    }

    #[inline]
    pub fn get(&self, key: &Symbol) -> Option<&Variable> {
        match &self.0 {
            Repr::Small(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == key.as_str())
                .map(|(_, v)| v),
            Repr::Large(map) => map.get(key),
        }
    }

    #[inline]
    pub fn get_str(&self, key: &str) -> Option<&Variable> {
        match &self.0 {
            Repr::Small(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| v),
            Repr::Large(map) => map.get(key),
        }
    }

    pub fn get_mut(&mut self, key: &Symbol) -> Option<&mut Variable> {
        match &mut self.0 {
            Repr::Small(entries) => entries
                .iter_mut()
                .find(|(k, _)| k.as_str() == key.as_str())
                .map(|(_, v)| v),
            Repr::Large(map) => map.get_mut(key),
        }
    }

    pub fn get_key_value(&self, key: &Symbol) -> Option<(&Symbol, &Variable)> {
        match &self.0 {
            Repr::Small(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == key.as_str())
                .map(|(k, v)| (k, v)),
            Repr::Large(map) => map.get_key_value(key),
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
            Repr::Small(entries) => entries
                .iter()
                .position(|(k, _)| k.as_str() == key)
                .map(|index| entries.remove(index).1),
            Repr::Large(map) => map.remove(key),
        }
    }

    pub fn insert(&mut self, key: Symbol, value: Variable) -> Option<Variable> {
        match &mut self.0 {
            Repr::Small(entries) => {
                if let Some(slot) = entries.iter_mut().find(|(k, _)| k.as_str() == key.as_str()) {
                    return Some(std::mem::replace(&mut slot.1, value));
                }
                if entries.len() >= SPILL_AT {
                    self.spill();
                    let Repr::Large(map) = &mut self.0 else {
                        unreachable!("just spilled")
                    };
                    return map.insert(key, value);
                }
                entries.push((key, value));
                None
            }
            Repr::Large(map) => map.insert(key, value),
        }
    }

    pub fn remove(&mut self, key: &Symbol) -> Option<Variable> {
        match &mut self.0 {
            Repr::Small(entries) => entries
                .iter()
                .position(|(k, _)| k.as_str() == key.as_str())
                .map(|index| entries.remove(index).1),
            Repr::Large(map) => map.remove(key),
        }
    }

    fn spill(&mut self) {
        let Repr::Small(entries) = &mut self.0 else {
            return;
        };
        let mut map = HashMap::with_capacity(entries.len() * 2);
        for (key, value) in entries.drain(..) {
            map.insert(key, value);
        }
        self.0 = Repr::Large(map);
    }

    pub fn entry(&mut self, key: Symbol) -> Entry<'_> {
        if matches!(&self.0, Repr::Small(entries)
            if entries.len() >= SPILL_AT && self.get(&key).is_none())
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
            Repr::Small(entries) => Iter::Small(entries.iter()),
            Repr::Large(map) => Iter::Large(map.iter()),
        }
    }

    pub fn iter_mut(&mut self) -> IterMut<'_> {
        match &mut self.0 {
            Repr::Small(entries) => IterMut::Small(entries.iter_mut()),
            Repr::Large(map) => IterMut::Large(map.iter_mut()),
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
    Small(std::slice::Iter<'a, (Symbol, Variable)>),
    Large(std::collections::hash_map::Iter<'a, Symbol, Variable>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Symbol, &'a Variable);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Iter::Small(iter) => iter.next().map(|(key, value)| (key, value)),
            Iter::Large(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Iter::Small(iter) => iter.size_hint(),
            Iter::Large(iter) => iter.size_hint(),
        }
    }
}

pub enum IterMut<'a> {
    Small(std::slice::IterMut<'a, (Symbol, Variable)>),
    Large(std::collections::hash_map::IterMut<'a, Symbol, Variable>),
}

impl<'a> Iterator for IterMut<'a> {
    type Item = (&'a Symbol, &'a mut Variable);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            IterMut::Small(iter) => iter.next().map(|(key, value)| (&*key, value)),
            IterMut::Large(iter) => iter.next(),
        }
    }
}

pub enum IntoIter {
    Small(smallvec::IntoIter<[(Symbol, Variable); INLINE]>),
    Large(std::collections::hash_map::IntoIter<Symbol, Variable>),
}

impl Iterator for IntoIter {
    type Item = (Symbol, Variable);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            IntoIter::Small(iter) => iter.next(),
            IntoIter::Large(iter) => iter.next(),
        }
    }
}

impl IntoIterator for VariableMap {
    type Item = (Symbol, Variable);
    type IntoIter = IntoIter;

    fn into_iter(self) -> IntoIter {
        match self.0 {
            Repr::Small(entries) => IntoIter::Small(entries.into_iter()),
            Repr::Large(map) => IntoIter::Large(map.into_iter()),
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

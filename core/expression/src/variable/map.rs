use std::borrow::Borrow;

use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;

#[derive(Debug, PartialEq, Eq, PartialOrd, Clone)]
pub struct BumpMap<'arena, K, V> {
    inner: BumpVec<'arena, (K, V)>,
}

impl<'arena, K, V> BumpMap<'arena, K, V> {
    pub fn new_in(arena: &'arena Bump) -> Self {
        BumpMap {
            inner: BumpVec::new_in(arena),
        }
    }

    pub fn with_capacity_in(capacity: usize, arena: &'arena Bump) -> Self {
        BumpMap {
            inner: BumpVec::with_capacity_in(capacity, arena),
        }
    }

    pub fn from_iter_in<I: IntoIterator<Item = (K, V)>>(iter: I, arena: &'arena Bump) -> Self {
        BumpMap {
            inner: BumpVec::from_iter_in(iter, arena),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.inner.iter().map(|(k, v)| (k, v))
    }

    fn position_of<Q>(&self, key: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        self.inner.iter().position(|(k, _)| k.borrow() == key)
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        self.position_of(key).is_some()
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        self.position_of(key.borrow()).map(|p| &self.inner[p].1)
    }

    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Eq + ?Sized,
    {
        self.position_of(key).map(|p| &mut self.inner[p].1)
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Eq,
    {
        let existing_position = self.position_of(&key);
        self.inner.push((key, value));

        existing_position.and_then(|p| Some(self.inner.remove(p).1))
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    pub fn clear(&mut self) {
        self.inner.clear()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn into_inner(self) -> BumpVec<'arena, (K, V)> {
        self.inner
    }
}

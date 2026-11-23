use std::cmp::Ordering;
use std::collections::{btree_set, BTreeSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::slice;

#[derive(Clone)]
pub struct Strings(Repr);

#[derive(Clone)]
#[allow(clippy::box_collection)]
enum Repr {
    One(Rc<str>),
    Few(Box<[Rc<str>]>),
    Many(Box<BTreeSet<Rc<str>>>),
}

impl Strings {
    const FEW: usize = 32;

    pub fn new() -> Self {
        Self(Repr::Few(Box::default()))
    }

    pub fn one(value: Rc<str>) -> Self {
        Self(Repr::One(value))
    }

    fn sorted(values: Vec<Rc<str>>) -> Self {
        if values.len() > Self::FEW {
            return Self(Repr::Many(Box::new(values.into_iter().collect())));
        }
        match <[Rc<str>; 1]>::try_from(values) {
            Ok([only]) => Self(Repr::One(only)),
            Err(values) => Self(Repr::Few(values.into_boxed_slice())),
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::One(_) => 1,
            Repr::Few(values) => values.len(),
            Repr::Many(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter(match &self.0 {
            Repr::One(only) => Walk::List(slice::from_ref(only).iter()),
            Repr::Few(values) => Walk::List(values.iter()),
            Repr::Many(values) => Walk::Tree(values.iter()),
        })
    }

    pub fn contains(&self, value: &str) -> bool {
        match &self.0 {
            Repr::One(only) => &**only == value,
            Repr::Few(values) => values.binary_search_by(|held| (**held).cmp(value)).is_ok(),
            Repr::Many(values) => values.contains(value),
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        let mut out = Vec::with_capacity(self.len() + other.len());
        let mut left = self.iter().peekable();
        let mut right = other.iter().peekable();
        loop {
            let order = match (left.peek(), right.peek()) {
                (Some(a), Some(b)) => a.cmp(b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => break,
            };
            match order {
                Ordering::Less => out.extend(left.next().cloned()),
                Ordering::Greater => out.extend(right.next().cloned()),
                Ordering::Equal => {
                    out.extend(left.next().cloned());
                    right.next();
                }
            }
        }
        Self::sorted(out)
    }

    pub fn intersection(&self, other: &Self) -> Self {
        let (small, large) = if self.len() <= other.len() {
            (self, other)
        } else {
            (other, self)
        };
        Self::sorted(
            small
                .iter()
                .filter(|value| large.contains(value))
                .cloned()
                .collect(),
        )
    }

    pub fn difference(&self, other: &Self) -> Self {
        if other.is_empty() {
            return self.clone();
        }
        Self::sorted(
            self.iter()
                .filter(|value| !other.contains(value))
                .cloned()
                .collect(),
        )
    }
}

impl Default for Strings {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for Strings {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Eq for Strings {}

impl Hash for Strings {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.len());
        for value in self {
            value.hash(state);
        }
    }
}

impl fmt::Debug for Strings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self).finish()
    }
}

impl FromIterator<Rc<str>> for Strings {
    fn from_iter<I: IntoIterator<Item = Rc<str>>>(values: I) -> Self {
        let mut values: Vec<Rc<str>> = values.into_iter().collect();
        if !values.windows(2).all(|pair| pair[0] < pair[1]) {
            values.sort_unstable();
            values.dedup();
        }
        Self::sorted(values)
    }
}

impl From<BTreeSet<Rc<str>>> for Strings {
    fn from(values: BTreeSet<Rc<str>>) -> Self {
        if values.len() > Self::FEW {
            return Self(Repr::Many(Box::new(values)));
        }
        Self::sorted(values.into_iter().collect())
    }
}

impl Extend<Rc<str>> for Strings {
    fn extend<I: IntoIterator<Item = Rc<str>>>(&mut self, values: I) {
        if let Repr::Many(held) = &mut self.0 {
            held.extend(values);
            return;
        }
        let mut all: Vec<Rc<str>> = self.iter().cloned().collect();
        all.extend(values);
        *self = all.into_iter().collect();
    }
}

impl<'a> IntoIterator for &'a Strings {
    type Item = &'a Rc<str>;
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

#[derive(Clone)]
pub struct Iter<'a>(Walk<'a>);

#[derive(Clone)]
enum Walk<'a> {
    List(slice::Iter<'a, Rc<str>>),
    Tree(btree_set::Iter<'a, Rc<str>>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a Rc<str>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            Walk::List(values) => values.next(),
            Walk::Tree(values) => values.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.0 {
            Walk::List(values) => values.size_hint(),
            Walk::Tree(values) => values.size_hint(),
        }
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            Walk::List(values) => values.next_back(),
            Walk::Tree(values) => values.next_back(),
        }
    }
}

impl ExactSizeIterator for Iter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, limit: usize) -> usize {
            (self.next() % limit as u64) as usize
        }

        fn values(&mut self, pool: &[Rc<str>]) -> Vec<Rc<str>> {
            let size = match self.below(6) {
                0 => 0,
                1 => 1,
                2 => 2 + self.below(6),
                3 => Strings::FEW - 1 + self.below(3),
                _ => self.below(pool.len()),
            };
            (0..size)
                .map(|_| pool[self.below(pool.len())].clone())
                .collect()
        }
    }

    fn pool(size: usize) -> Vec<Rc<str>> {
        (0..size)
            .map(|i| Rc::from(format!("v{:03}", (i * 37) % size)))
            .collect()
    }

    fn hash_of(value: &impl Hash) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    fn listed(set: &Strings) -> Vec<Rc<str>> {
        set.iter().cloned().collect()
    }

    fn expected<'a>(values: impl Iterator<Item = &'a Rc<str>>) -> Vec<Rc<str>> {
        values.cloned().collect()
    }

    #[test]
    fn matches_a_tree_set_on_random_inputs() {
        let pool = pool(120);
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for _ in 0..4_000 {
            let (left, right) = (random.values(&pool), random.values(&pool));
            let (a, b): (Strings, Strings) = (
                left.iter().cloned().collect(),
                right.iter().cloned().collect(),
            );
            let (x, y): (BTreeSet<Rc<str>>, BTreeSet<Rc<str>>) = (
                left.iter().cloned().collect(),
                right.iter().cloned().collect(),
            );

            assert_eq!(listed(&a), expected(x.iter()));
            assert_eq!(a.len(), x.len());
            assert_eq!(a.is_empty(), x.is_empty());
            assert_eq!(listed(&a.union(&b)), expected(x.union(&y)));
            assert_eq!(listed(&a.intersection(&b)), expected(x.intersection(&y)));
            assert_eq!(listed(&a.difference(&b)), expected(x.difference(&y)));
            assert_eq!(
                a.iter().rev().cloned().collect::<Vec<_>>(),
                x.iter().rev().cloned().collect::<Vec<_>>()
            );
            for value in &pool {
                assert_eq!(a.contains(value), x.contains(value));
            }
            assert_eq!(a == b, x == y);
            assert_eq!(hash_of(&a), hash_of(&x));
            assert_eq!(format!("{a:?}"), format!("{x:?}"));
            assert_eq!(Strings::from(x.clone()), a);

            let mut grown = a.clone();
            grown.extend(right.iter().cloned());
            assert_eq!(grown, a.union(&b));
            assert_eq!(hash_of(&grown), hash_of(&a.union(&b)));
        }
    }

    #[test]
    fn grows_one_value_at_a_time_across_every_size() {
        let pool = pool(200);
        let mut set = Strings::new();
        let mut tree = BTreeSet::new();
        for value in &pool {
            set.extend([value.clone()]);
            tree.insert(value.clone());
            assert_eq!(listed(&set), expected(tree.iter()));
            assert_eq!(set, tree.iter().cloned().collect::<Strings>());
        }
    }

    #[test]
    fn stays_three_words_wide() {
        assert!(size_of::<Strings>() <= 3 * size_of::<usize>());
    }
}

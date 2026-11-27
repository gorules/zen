use std::hash::Hash;

pub(crate) struct Specialized<K, V> {
    cap: usize,
    cache: ahash::HashMap<K, V>,
}

impl<K: Hash + Eq, V: Clone> Specialized<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            cache: ahash::HashMap::default(),
        }
    }

    pub fn get(&self, key: &K) -> Option<V> {
        self.cache.get(key).cloned()
    }

    pub fn insert(&mut self, key: K, value: V) {
        if self.cache.len() > self.cap {
            self.cache.clear();
        }
        self.cache.insert(key, value);
    }

    pub fn get_or_insert_with(&mut self, key: K, build: impl FnOnce() -> V) -> V {
        if self.cache.len() > self.cap && !self.cache.contains_key(&key) {
            self.cache.clear();
        }
        self.cache.entry(key).or_insert_with(build).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::Specialized;

    #[test]
    fn clears_once_past_the_cap() {
        let mut cache: Specialized<u32, u32> = Specialized::new(2);
        (0..3).for_each(|key| cache.insert(key, key));
        assert_eq!(cache.get(&0), Some(0));
        cache.insert(3, 3);
        assert_eq!(cache.get(&0), None);
        assert_eq!(cache.get(&3), Some(3));
    }

    #[test]
    fn get_or_insert_with_keeps_failures_and_existing_entries() {
        let mut cache: Specialized<u32, Option<u32>> = Specialized::new(1);
        assert_eq!(cache.get_or_insert_with(1, || None), None);
        assert_eq!(cache.get_or_insert_with(1, || Some(5)), None);
        assert_eq!(cache.get_or_insert_with(2, || Some(2)), Some(2));
        assert_eq!(cache.get_or_insert_with(1, || Some(7)), None);
        assert_eq!(cache.get_or_insert_with(3, || Some(3)), Some(3));
        assert_eq!(cache.get(&1), None);
    }
}

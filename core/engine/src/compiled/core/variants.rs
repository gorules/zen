use std::sync::RwLock;

pub(crate) struct Variants<V> {
    cap: usize,
    list: RwLock<Vec<V>>,
}

impl<V> Variants<V> {
    pub(crate) fn new(cap: usize) -> Self {
        Self { cap, list: RwLock::new(Vec::new()) }
    }

    pub(crate) fn find_map<T>(&self, find: impl FnMut(&V) -> Option<T>) -> Option<T> {
        self.list.read().ok().and_then(|list| list.iter().find_map(find))
    }

    pub(crate) fn keep(&self, variant: V) {
        if let Ok(mut list) = self.list.write() {
            if list.len() < self.cap {
                list.push(variant);
            }
        }
    }

    pub(crate) fn get_or_build<T>(&self, find: impl Fn(&V) -> Option<T>, build: impl FnOnce() -> V, overflow: impl FnOnce() -> V) -> Option<T> {
        if let Some(found) = self.find_map(&find) {
            return Some(found);
        }
        let Ok(mut list) = self.list.write() else {
            return find(&overflow());
        };
        if let Some(found) = list.iter().find_map(&find) {
            return Some(found);
        }
        let variant = match list.len() < self.cap {
            true => build(),
            false => overflow(),
        };
        let found = find(&variant);
        list.push(variant);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::Variants;
    use std::cell::Cell;

    #[test]
    fn kept_variants_replay_in_insertion_order_up_to_the_cap() {
        let variants: Variants<(u32, &str)> = Variants::new(2);
        variants.keep((1, "a"));
        variants.keep((1, "b"));
        variants.keep((2, "c"));
        assert_eq!(variants.find_map(|&(key, name)| (key == 1).then_some(name)), Some("a"));
        assert_eq!(variants.find_map(|&(key, name)| (key == 2).then_some(name)), None);
    }

    #[test]
    fn built_variants_are_cached_and_overflow_is_cached_too() {
        let variants: Variants<(u32, u32)> = Variants::new(1);
        let built = Cell::new(0);
        let find = |key: u32| move |&(k, v): &(u32, u32)| (k == key).then_some(v);
        let build = |key: u32| {
            built.set(built.get() + 1);
            (key, key * 10)
        };
        assert_eq!(variants.get_or_build(find(1), || build(1), || (1, 0)), Some(10));
        assert_eq!(variants.get_or_build(find(1), || build(1), || (1, 0)), Some(10));
        assert_eq!(built.get(), 1);
        assert_eq!(variants.get_or_build(find(2), || build(2), || (2, 0)), Some(0));
        assert_eq!(variants.get_or_build(find(2), || build(2), || (2, 99)), Some(0));
        assert_eq!(built.get(), 1);
    }
}

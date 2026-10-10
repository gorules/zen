use std::borrow::Borrow;
use std::sync::RwLock;

pub(crate) struct Children<K, S> {
    cache: RwLock<Vec<(K, S)>>,
}

impl<K, S: Clone> Children<K, S> {
    pub(crate) fn new() -> Self {
        Self { cache: RwLock::new(Vec::new()) }
    }

    pub(crate) fn get<Q>(&self, key: &Q, build: impl FnOnce() -> S) -> S
    where
        K: Borrow<Q>,
        Q: PartialEq + ToOwned<Owned = K> + ?Sized,
    {
        let found = |cache: &[(K, S)]| cache.iter().find(|(k, _)| k.borrow() == key).map(|(_, child)| child.clone());
        if let Some(child) = self.cache.read().ok().and_then(|cache| found(&cache)) {
            return child;
        }
        let built = build();
        let Ok(mut cache) = self.cache.write() else {
            return built;
        };
        if let Some(child) = found(&cache) {
            return child;
        }
        cache.push((key.to_owned(), built.clone()));
        built
    }

    pub(crate) fn each<E>(&self, mut visit: impl FnMut(&S) -> Result<(), E>) -> Result<(), E> {
        match self.cache.read() {
            Ok(cache) => cache.iter().try_for_each(|(_, child)| visit(child)),
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Children;
    use std::cell::Cell;

    #[test]
    fn builds_each_key_once_and_visits_in_insertion_order() {
        let children: Children<Vec<u64>, u32> = Children::new();
        let built = Cell::new(0);
        let build = |value: u32| {
            built.set(built.get() + 1);
            value
        };
        assert_eq!(children.get(&[2][..], || build(20)), 20);
        assert_eq!(children.get(&[1][..], || build(10)), 10);
        assert_eq!(children.get(&[2][..], || build(99)), 20);
        assert_eq!(built.get(), 2);
        let mut seen = Vec::new();
        let visited: Result<(), ()> = children.each(|child| {
            seen.push(*child);
            Ok(())
        });
        assert_eq!(visited, Ok(()));
        assert_eq!(seen, vec![20, 10]);
        assert_eq!(children.each(|child| if *child == 20 { Err(*child) } else { Ok(()) }), Err(20));
    }
}

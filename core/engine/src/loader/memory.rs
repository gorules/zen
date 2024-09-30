use crate::loader::{DecisionLoader, LoaderError, LoaderResponse};
use crate::model::DecisionContent;
use ahash::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};

/// Loads decisions from in-memory hashmap
#[derive(Debug, Default)]
pub struct MemoryLoader {
    memory_refs: RwLock<HashMap<String, Arc<DecisionContent>>>,
}

impl MemoryLoader {
    pub fn add<K, D>(&self, key: K, content: D)
    where
        K: Into<String>,
        D: Into<DecisionContent>,
    {
        let mut mref = self.memory_refs.write().unwrap();
        mref.insert(key.into(), Arc::new(content.into()));
    }

    pub fn get<K>(&self, key: K) -> Option<Arc<DecisionContent>>
    where
        K: AsRef<str>,
    {
        let mref = self.memory_refs.read().unwrap();
        mref.get(key.as_ref()).map(|r| r.clone())
    }

    pub fn remove<K>(&self, key: K) -> bool
    where
        K: AsRef<str>,
    {
        let mut mref = self.memory_refs.write().unwrap();
        mref.remove(key.as_ref()).is_some()
    }
}

impl DecisionLoader for MemoryLoader {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            self.get(&key)
                .ok_or_else(|| LoaderError::NotFound(key.to_string()).into())
        }
    }
}

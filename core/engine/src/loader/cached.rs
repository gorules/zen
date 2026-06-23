use ahash::{HashMap, HashMapExt};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::loader::{DecisionLoader, DynamicLoader, LoaderResponse};
use crate::model::DecisionContent;

#[derive(Debug)]
pub struct CachedLoader {
    loader: DynamicLoader,
    cache: Mutex<HashMap<String, Arc<DecisionContent>>>,
}

impl From<DynamicLoader> for CachedLoader {
    fn from(value: DynamicLoader) -> Self {
        Self {
            loader: value,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl DecisionLoader for CachedLoader {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move {
            let mut cache = self.cache.lock().await;
            if let Some(content) = cache.get(key) {
                return Ok(content.clone());
            }

            let decision_content = self.loader.load(key).await?;
            cache.insert(key.to_string(), decision_content.clone());
            Ok(decision_content)
        })
    }

    fn keys(&self) -> Option<Vec<Arc<str>>> {
        self.loader.keys()
    }

    fn load_sync(&self, key: &str) -> Option<LoaderResponse> {
        let Ok(mut cache) = self.cache.try_lock() else {
            return self.loader.load_sync(key);
        };
        if let Some(content) = cache.get(key) {
            return Some(Ok(content.clone()));
        }
        let response = self.loader.load_sync(key)?;
        if let Ok(content) = &response {
            cache.insert(key.to_string(), content.clone());
        }
        Some(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::MemoryLoader;
    use crate::model::DecisionContent;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct CountingLoader {
        inner: MemoryLoader,
        sync_loads: AtomicUsize,
    }

    impl DecisionLoader for CountingLoader {
        fn load<'a>(
            &'a self,
            key: &'a str,
        ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
            self.inner.load(key)
        }

        fn load_sync(&self, key: &str) -> Option<LoaderResponse> {
            self.sync_loads.fetch_add(1, Ordering::SeqCst);
            self.inner.load_sync(key)
        }
    }

    #[test]
    fn load_sync_uses_cache_and_hits_inner_once() {
        let counting = Arc::new(CountingLoader::default());
        counting.inner.add("graph.json", DecisionContent::default());
        let cached = CachedLoader::from(counting.clone() as DynamicLoader);

        let first = cached.load_sync("graph.json").unwrap().unwrap();
        let second = cached.load_sync("graph.json").unwrap().unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(counting.sync_loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn delegates_keys_and_load_sync_to_inner_loader() {
        let memory_loader = MemoryLoader::default();
        memory_loader.add("graph.json", DecisionContent::default());

        let cached = CachedLoader::from(Arc::new(memory_loader) as DynamicLoader);

        let keys = cached.keys().unwrap();
        assert_eq!(keys, vec![Arc::from("graph.json")]);

        let content = cached.load_sync("graph.json").unwrap().unwrap();
        assert!(content.as_graph().is_some());

        assert!(cached.load_sync("missing.json").unwrap().is_err());
    }
}

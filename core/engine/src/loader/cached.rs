use ahash::{HashMap, HashMapExt};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::loader::{DecisionLoader, DynamicLoader, LoaderResponse};
use crate::model::DecisionContent;

#[derive(Debug)]
pub struct CachedLoader {
    loader: DynamicLoader,
    cache: RwLock<HashMap<String, Arc<DecisionContent>>>,
}

impl From<DynamicLoader> for CachedLoader {
    fn from(value: DynamicLoader) -> Self {
        Self {
            loader: value,
            cache: RwLock::new(HashMap::new()),
        }
    }
}

fn compiled(content: Arc<DecisionContent>) -> Arc<DecisionContent> {
    let DecisionContent::Graph(graph) = content.as_ref() else {
        return content;
    };
    if graph.compiled_cache.is_some() {
        return content;
    }

    let mut owned = (**graph).clone();
    owned.compile();
    Arc::new(DecisionContent::Graph(Arc::new(owned)))
}

async fn prepared(loader: &DynamicLoader, content: Arc<DecisionContent>) -> Arc<DecisionContent> {
    let DecisionContent::Graph(graph) = content.as_ref() else {
        return content;
    };
    if graph.compiled_cache.is_some() && graph.resolved_schemas.is_some() {
        return content;
    }

    let mut owned = (**graph).clone();
    owned.compile();
    let _ = owned.resolve_schemas(loader).await;
    Arc::new(DecisionContent::Graph(Arc::new(owned)))
}

impl DecisionLoader for CachedLoader {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move {
            let cached = self
                .cache
                .read()
                .ok()
                .and_then(|cache| cache.get(key).cloned());

            let loaded = match &cached {
                Some(content) => content.clone(),
                None => self.loader.load(key).await?,
            };

            let decision_content = prepared(&self.loader, loaded).await;
            let unchanged = cached
                .as_ref()
                .is_some_and(|content| Arc::ptr_eq(content, &decision_content));
            if !unchanged {
                if let Ok(mut cache) = self.cache.write() {
                    cache.insert(key.to_string(), decision_content.clone());
                }
            }
            Ok(decision_content)
        })
    }

    fn keys(&self) -> Option<Vec<Arc<str>>> {
        self.loader.keys()
    }

    fn load_sync(&self, key: &str) -> Option<LoaderResponse> {
        if let Ok(cache) = self.cache.read() {
            if let Some(content) = cache.get(key) {
                return Some(Ok(content.clone()));
            }
        }

        let response = self.loader.load_sync(key)?.map(compiled);
        if let Ok(content) = &response {
            if let Ok(mut cache) = self.cache.write() {
                cache.insert(key.to_string(), content.clone());
            }
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

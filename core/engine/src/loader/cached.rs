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
    fn load<'a>(&'a self, key: &'a str) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a>> {
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
}

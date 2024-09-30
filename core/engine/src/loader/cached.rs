use ahash::{HashMap, HashMapExt};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::loader::{DecisionLoader, LoaderResponse};
use crate::model::DecisionContent;

pub struct CachedLoader<Loader: DecisionLoader + 'static> {
    loader: Arc<Loader>,
    cache: Mutex<HashMap<String, Arc<DecisionContent>>>,
}

impl<Loader: DecisionLoader + 'static> From<Arc<Loader>> for CachedLoader<Loader> {
    fn from(value: Arc<Loader>) -> Self {
        Self {
            loader: value,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl<Loader: DecisionLoader + 'static> DecisionLoader for CachedLoader<Loader> {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            let mut cache = self.cache.lock().await;
            if let Some(content) = cache.get(key) {
                return Ok(content.clone());
            }

            let decision_content = self.loader.load(key).await?;
            cache.insert(key.to_string(), decision_content.clone());
            Ok(decision_content)
        }
    }
}

use crate::EvaluationError;
use ahash::HashMap;
use jsonschema::Validator;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Default, Debug)]
pub struct ValidatorCache {
    inner: Arc<RwLock<HashMap<u64, Arc<Validator>>>>,
}

impl ValidatorCache {
    pub async fn get(&self, key: u64) -> Option<Arc<Validator>> {
        let read = self.inner.read().await;
        read.get(&key).cloned()
    }

    pub async fn get_or_insert(
        &self,
        key: u64,
        schema: &Value,
    ) -> Result<Arc<Validator>, Box<EvaluationError>> {
        if let Some(v) = self.get(key).await {
            return Ok(v);
        }

        let mut w_shared = self.inner.write().await;
        let validator = Arc::new(jsonschema::draft7::new(&schema)?);
        w_shared.insert(key, validator.clone());

        Ok(validator)
    }
}

use ahash::HashMap;
use anyhow::Context;
use jsonschema::Validator;
use serde_json::Value;
use std::sync::{Arc, RwLock};

#[derive(Clone, Default, Debug)]
pub struct ValidatorCache {
    inner: Arc<RwLock<HashMap<u64, Arc<Validator>>>>,
}

impl ValidatorCache {
    pub fn get(&self, key: u64) -> Option<Arc<Validator>> {
        let read = self.inner.read().ok()?;
        read.get(&key).cloned()
    }

    pub fn get_or_insert(&self, key: u64, schema: &Value) -> anyhow::Result<Arc<Validator>> {
        if let Some(v) = self.get(key) {
            return Ok(v);
        }

        let mut w_shared = self
            .inner
            .write()
            .ok()
            .context("Failed to acquire lock on validator cache")?;
        let validator = Arc::new(jsonschema::draft7::new(&schema)?);
        w_shared.insert(key, validator.clone());

        Ok(validator)
    }
}

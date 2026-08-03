use crate::nodes::variable_json::VariableJson;
use ahash::HashMap;
use anyhow::Context;
use jsonschema::Validator;
use serde_json::Value;
use std::sync::{Arc, RwLock};

#[derive(Clone, Default, Debug)]
pub struct ValidatorCache {
    inner: Arc<RwLock<HashMap<u64, Arc<Validator<VariableJson>>>>>,
}

impl PartialEq for ValidatorCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl ValidatorCache {
    pub fn get(&self, key: u64) -> Option<Arc<Validator<VariableJson>>> {
        let read = self.inner.read().ok()?;
        read.get(&key).cloned()
    }

    pub fn get_or_insert(
        &self,
        key: u64,
        schema: &Value,
    ) -> anyhow::Result<Arc<Validator<VariableJson>>> {
        if let Some(v) = self.get(key) {
            return Ok(v);
        }

        let mut w_shared = self
            .inner
            .write()
            .ok()
            .context("Failed to acquire lock on validator cache")?;
        let validator = Arc::new(
            jsonschema::options_for::<VariableJson>()
                .with_draft(jsonschema::Draft::Draft7)
                .build(schema)?,
        );
        w_shared.insert(key, validator.clone());

        Ok(validator)
    }
}

use crate::compiled::host::variable_json::VariableJson;
pub(crate) use crate::nodes::validator_cache::ValidatorCache;
use ahash::AHasher;
use jsonschema::Validator;
use serde_json::Value;
use std::hash::Hasher;

pub(crate) struct Validators;

impl Validators {
    pub(crate) fn key(id: &str, name: &str, salt: u64) -> u64 {
        let mut hasher = AHasher::default();
        hasher.write(id.as_bytes());
        hasher.write(name.as_bytes());
        hasher.write_u64(salt);
        hasher.finish()
    }

    pub(crate) fn build(schema: &Value) -> anyhow::Result<Validator<VariableJson>> {
        Ok(jsonschema::options_for::<VariableJson>()
            .with_draft(jsonschema::Draft::Draft7)
            .with_format("date", zen_expression::DateValue::is_text)
            .with_format("date-time", zen_expression::DateValue::is_text)
            .build(schema)?)
    }
}

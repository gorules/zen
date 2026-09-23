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
        nullable_optionals: bool,
    ) -> anyhow::Result<Arc<Validator<VariableJson>>> {
        if let Some(v) = self.get(key) {
            return Ok(v);
        }

        let mut w_shared = self
            .inner
            .write()
            .ok()
            .context("Failed to acquire lock on validator cache")?;
        let rewritten;
        let schema = if nullable_optionals {
            rewritten = NullableOptionals::apply(schema);
            &rewritten
        } else {
            schema
        };
        let validator = Arc::new(
            jsonschema::options_for::<VariableJson>()
                .with_draft(jsonschema::Draft::Draft7)
                .build(schema)?,
        );
        w_shared.insert(key, validator.clone());

        Ok(validator)
    }
}

pub(crate) struct NullableOptionals;

impl NullableOptionals {
    const SCHEMA_MAPS: &[&str] = &["properties", "patternProperties", "definitions", "$defs"];
    const SCHEMA_LISTS: &[&str] = &["anyOf", "oneOf", "allOf"];
    const SCHEMA_ONES: &[&str] = &["additionalProperties", "not", "if", "then", "else"];
    const COMPOSITE: &[&str] = &["$ref", "const", "anyOf", "oneOf", "allOf", "not", "if"];

    pub(crate) fn apply(schema: &Value) -> Value {
        let mut schema = schema.clone();
        Self::rewrite(&mut schema);
        schema
    }

    fn rewrite(schema: &mut Value) {
        let Some(object) = schema.as_object_mut() else {
            return;
        };
        for key in Self::SCHEMA_MAPS {
            if let Some(map) = object.get_mut(*key).and_then(Value::as_object_mut) {
                map.values_mut().for_each(Self::rewrite);
            }
        }
        for key in Self::SCHEMA_LISTS {
            if let Some(list) = object.get_mut(*key).and_then(Value::as_array_mut) {
                list.iter_mut().for_each(Self::rewrite);
            }
        }
        for key in Self::SCHEMA_ONES {
            if let Some(child) = object.get_mut(*key) {
                Self::rewrite(child);
            }
        }
        match object.get_mut("items") {
            Some(Value::Array(items)) => items.iter_mut().for_each(Self::rewrite),
            Some(items) => Self::rewrite(items),
            None => {}
        }

        let required: Vec<String> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
            for (name, property) in properties.iter_mut() {
                if !required.contains(name) {
                    Self::allow_null(property);
                }
            }
        }
    }

    fn allow_null(schema: &mut Value) {
        let Some(object) = schema.as_object_mut() else {
            return;
        };
        if Self::COMPOSITE.iter().any(|key| object.contains_key(*key)) {
            let original = std::mem::take(schema);
            *schema = serde_json::json!({ "anyOf": [{ "type": "null" }, original] });
            return;
        }
        match object.get_mut("type") {
            Some(Value::String(kind)) if kind != "null" => {
                let kind = std::mem::take(kind);
                object.insert("type".into(), serde_json::json!([kind, "null"]));
            }
            Some(Value::Array(kinds)) if !kinds.iter().any(|k| k == "null") => {
                kinds.push(Value::from("null"));
            }
            _ => {}
        }
        if let Some(values) = object.get_mut("enum").and_then(Value::as_array_mut) {
            if !values.iter().any(Value::is_null) {
                values.push(Value::Null);
            }
        }
    }
}

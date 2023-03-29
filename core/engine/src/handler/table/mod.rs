pub mod zen;

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Debug)]
pub enum RowOutputKind {
    Value(Value),
}

#[derive(Debug, Default)]
pub struct RowOutput {
    output: OutputMap,
}

#[derive(Debug, Error)]
pub enum RowOutputError {
    #[error("Failed to parse")]
    FailedToParse,
}

type OutputMap = HashMap<String, RowOutputKind>;

impl Deref for RowOutput {
    type Target = OutputMap;

    fn deref(&self) -> &Self::Target {
        &self.output
    }
}

impl DerefMut for RowOutput {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.output
    }
}

impl RowOutput {
    pub async fn to_json(&self) -> Result<Value, RowOutputError> {
        let mut hmap: HashMap<String, Value> = Default::default();
        self.output.iter().try_for_each(|(key, kind)| match kind {
            RowOutputKind::Value(value) => {
                hmap.insert(key.clone(), value.clone());
                Ok(())
            }
        })?;

        let json = unflatten_json(&hmap, ".");
        serde_json::to_value(&json).map_err(|_| RowOutputError::FailedToParse)
    }
}

fn unflatten_json(data: &HashMap<String, Value>, separator: &str) -> Value {
    let mut result = Value::Object(Map::new());

    'outer: for (key, value) in data {
        let mut obj = &mut result;

        for part in key.split(separator) {
            if !obj.is_object() {
                continue 'outer;
            }

            obj = obj
                .as_object_mut()
                .unwrap()
                .entry(part.to_string())
                .or_insert(Value::Object(Map::new()));
        }

        let _ = std::mem::replace(obj, value.clone());
    }

    result
}

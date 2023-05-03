pub mod zen;

use crate::util::json_map::{FlatJsonMap, JsonMapError};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) enum RowOutputKind {
    Value(Value),
}

#[derive(Debug, Default)]
pub(crate) struct RowOutput {
    output: OutputMap,
}

type OutputMap = Vec<(String, RowOutputKind)>;

impl RowOutput {
    pub fn push<K: Into<String>>(&mut self, key: K, value: RowOutputKind) {
        self.output.push((key.into(), value))
    }

    pub async fn to_json(&self) -> Result<Value, JsonMapError> {
        let map: Vec<(String, Value)> = self
            .output
            .iter()
            .map(|(key, kind)| match kind {
                RowOutputKind::Value(value) => (key.clone(), value.clone()),
            })
            .collect();

        FlatJsonMap::from(map).to_json()
    }
}

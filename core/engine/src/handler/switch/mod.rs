pub mod zen;

use crate::util::json_map::{FlatJsonMap, JsonMapError};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) enum RuleOutputKind {
    Value(Value),
}

#[derive(Debug, Default)]
pub(crate) struct RuleOutput {
    output: OutputMap,
}

// enum RuleValue {
//     Terminal(String),
//     Nested(HashMap<String, RuleValue>),
// }

// type Rules = HashMap<String, RuleValue>;

type OutputMap = Vec<(String, RuleOutputKind)>;

impl RuleOutput {
    pub fn push<K: Into<String>>(&mut self, key: K, value: RuleOutputKind) {
        self.output.push((key.into(), value))
    }

    pub async fn to_json(&self) -> Result<Value, JsonMapError> {
        let map: Vec<(String, Value)> = self
            .output
            .iter()
            .map(|(key, kind)| match kind {
                RuleOutputKind::Value(value) => (key.clone(), value.clone()),
            })
            .collect();

        FlatJsonMap::from(map).to_json()
    }
}

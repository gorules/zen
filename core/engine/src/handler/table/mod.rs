pub mod zen;

use serde::{Serialize, Serializer};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

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

    #[error("Unexpected serde error has occurred while deserializing rows")]
    SerdeError,
}

type OutputMap = Vec<(String, RowOutputKind)>;

impl RowOutput {
    pub fn push<K: Into<String>>(&mut self, key: K, value: RowOutputKind) {
        self.output.push((key.into(), value))
    }

    pub async fn to_json(&self) -> Result<Value, RowOutputError> {
        let map = self
            .output
            .iter()
            .map(|(key, kind)| match kind {
                RowOutputKind::Value(value) => (key, value),
            })
            .enumerate()
            .map(|(index, (key, value))| flatten_value(key, value.clone(), index as u32))
            .collect::<Vec<BTreeMap<RowKey, Value>>>();

        let mut result: BTreeMap<RowKey, Value> = BTreeMap::new();
        for inner_map in map {
            for (key, value) in inner_map {
                let rk = RowKey::from(key);
                match value {
                    // Unexpected, as we've filtered out all objects in prior step
                    Value::Object(_) => return Err(RowOutputError::FailedToParse),
                    Value::Array(arr) => {
                        let maybe_exist = result.get_mut(&rk).map(|a| a.as_array_mut()).flatten();
                        if let Some(exist) = maybe_exist {
                            exist.extend_from_slice(&arr);
                        } else {
                            result.insert(rk, Value::Array(arr));
                        }
                    }
                    _ => {
                        result.insert(rk, value);
                    }
                }
            }
        }

        let mut root = Map::new();
        for (key, value) in result {
            let mut node = &mut root;
            let mut segments = key
                .str
                .split('.')
                .map(|s| s.to_string())
                .collect::<Vec<String>>();

            let last_segment = segments.pop().ok_or_else(|| RowOutputError::SerdeError)?;

            for segment in segments {
                node = node
                    .entry(segment)
                    .and_modify(|val| {
                        if !val.is_object() {
                            let _ = std::mem::replace(val, Value::Object(Map::new()));
                        }
                    })
                    .or_insert(Value::Object(Map::new()))
                    .as_object_mut()
                    .ok_or_else(|| RowOutputError::SerdeError)?;
            }

            node.insert(last_segment, value);
        }

        Ok(Value::Object(root))
    }
}

fn flatten_value(prefix_key: &str, value: Value, bucket: u32) -> BTreeMap<RowKey, Value> {
    let mut map: BTreeMap<RowKey, Value> = BTreeMap::new();
    match value {
        Value::Object(obj) => {
            for (key, value) in obj {
                if let Value::Object(inner_obj) = value {
                    let inner_map = flatten_value(&key, Value::Object(inner_obj), bucket);
                    for (inner_key, inner_value) in inner_map {
                        map.insert(
                            RowKey::new(format!("{prefix_key}.{key}.{inner_key}"), bucket),
                            inner_value,
                        );
                    }
                } else {
                    map.insert(RowKey::new(format!("{prefix_key}.{key}"), bucket), value);
                }
            }
        }
        _ => {
            map.insert(RowKey::new(prefix_key, bucket), value);
        }
    }

    map
}

#[derive(Debug, Eq, PartialEq)]
struct RowKey {
    str: String,
    sep_occurrences: u32,
    bucket: u32,
}

impl RowKey {
    fn new<T: Into<String>>(value: T, bucket: u32) -> Self {
        let str = value.into();
        let sep_occurrences = str.chars().fold(0, |c, k| if k == '.' { c + 1 } else { c });

        Self {
            str,
            sep_occurrences,
            bucket,
        }
    }
}

impl PartialOrd for RowKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RowKey {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.bucket != other.bucket {
            return self.bucket.cmp(&other.bucket);
        }

        if self.sep_occurrences == other.sep_occurrences {
            self.str.cmp(&other.str)
        } else {
            self.sep_occurrences.cmp(&other.sep_occurrences)
        }
    }
}

impl Serialize for RowKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.str)
    }
}

impl Display for RowKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.str)
    }
}

#[cfg(test)]
mod tests {
    use crate::handler::table::{RowKey, RowOutput, RowOutputKind};
    use serde_json::json;
    use std::cmp::Ordering;

    macro_rules! rk {
        ($str: expr) => {
            RowKey::new($str, 0)
        };
        ($str: expr, $order: expr) => {
            RowKey::new($str, $order)
        };
    }

    #[test]
    fn test_order() {
        assert_eq!(rk!("a").cmp(&rk!("b")), Ordering::Less);
        assert_eq!(rk!("a.b").cmp(&rk!("b")), Ordering::Greater);
        assert_eq!(rk!("a").cmp(&rk!("b.a")), Ordering::Less);
        assert_eq!(rk!("a.b.c").cmp(&rk!("a.b.c")), Ordering::Equal);
        assert_eq!(rk!("a.b.c").cmp(&rk!("a.b.c", 1)), Ordering::Less);
    }

    #[test]
    fn test_insert_order() {
        let mut o = RowOutput::default();
        o.push("a", RowOutputKind::Value(json!("abc")));
        o.push("a.b", RowOutputKind::Value(json!("abc")));
        o.push("a.b.c", RowOutputKind::Value(json!("abc")));

        assert_eq!(
            tokio_test::block_on(o.to_json()).unwrap(),
            json!({ "a": { "b": { "c": "abc" } } })
        );
    }

    #[test]
    fn test_nested() {
        let mut o = RowOutput::default();
        o.push("a.first.deleted", RowOutputKind::Value(json!("deleted")));
        o.push(
            "a.third.firstNested",
            RowOutputKind::Value(json!("firstNested")),
        );
        o.push("a.first", RowOutputKind::Value(json!("first")));
        o.push("a.second", RowOutputKind::Value(json!("second")));
        o.push("a.third.nested", RowOutputKind::Value(json!("nested")));

        assert_eq!(
            tokio_test::block_on(o.to_json()).unwrap(),
            json!({
                "a": {
                    "first": "first",
                    "second": "second",
                    "third": {
                        "firstNested": "firstNested",
                        "nested": "nested"
                    }
                }
            })
        );
    }
}

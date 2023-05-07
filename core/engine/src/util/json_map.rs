#![allow(dead_code)]

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use thiserror::Error;

#[derive(Clone, Default, Debug)]
pub(crate) struct FlatJsonMap {
    inner: Vec<(String, Value)>,
}

impl From<Vec<(String, Value)>> for FlatJsonMap {
    fn from(value: Vec<(String, Value)>) -> Self {
        Self { inner: value }
    }
}

impl FlatJsonMap {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Vec::with_capacity(capacity),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    pub fn insert<T: Into<String>>(&mut self, key: T, value: Value) {
        self.inner.push((key.into(), value))
    }

    pub fn remove<T: AsRef<str>>(&mut self, key: T) {
        self.inner.retain(|(k, _)| k == key.as_ref())
    }

    pub fn reserve(&mut self, additional: usize) {
        self.inner.reserve(additional)
    }

    pub fn to_json(&self) -> Result<Value, JsonMapError> {
        let map = self
            .inner
            .iter()
            .enumerate()
            .map(|(index, (key, value))| flatten_value(key, value.clone(), index as u32))
            .collect::<Vec<BTreeMap<JsonMapKey, Value>>>();

        let mut result = BTreeMap::<JsonMapKey, Value>::new();
        for inner_map in map {
            for (key, value) in inner_map {
                match value {
                    // Unexpected, as we've filtered out all objects in prior step
                    Value::Object(_) => return Err(JsonMapError::FailedToParse),
                    Value::Array(arr) => {
                        let maybe_exist = result.get_mut(&key).map(|a| a.as_array_mut()).flatten();
                        if let Some(exist) = maybe_exist {
                            exist.extend_from_slice(&arr);
                        } else {
                            result.insert(key, Value::Array(arr));
                        }
                    }
                    _ => {
                        result.insert(key, value);
                    }
                }
            }
        }

        let mut root = Map::new();
        for (key, mut value) in result {
            let mut node = &mut root;
            let mut segments = key.str.split('.');
            let last_segment = segments.next_back().ok_or(JsonMapError::SerdeError)?;

            for segment in segments {
                node = node
                    .entry(segment)
                    .and_modify(|val| {
                        if !matches!(val, Value::Object(_)) {
                            let _ = std::mem::replace(val, Value::Object(Map::new()));
                        }
                    })
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .ok_or(JsonMapError::SerdeError)?;
            }

            let entry = node.get_mut(last_segment);
            if let Some(mut entry_val) = entry {
                match (&mut entry_val, &mut value) {
                    (Value::Array(arr1), Value::Array(arr2)) => arr1.extend_from_slice(&arr2),
                    _ => {
                        let _ = std::mem::replace(entry_val, value);
                    }
                }
            } else {
                node.insert(last_segment.to_string(), value);
            }
        }

        Ok(Value::Object(root))
    }
}

#[derive(Debug, Error)]
pub(crate) enum JsonMapError {
    #[error("Failed to parse")]
    FailedToParse,

    #[error("Unexpected serde error has occurred while deserializing rows")]
    SerdeError,
}

#[derive(Debug, Eq, PartialEq)]
struct JsonMapKey {
    str: String,
    sep_occurrences: u32,
    bucket: u32,
}

impl JsonMapKey {
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

impl PartialOrd for JsonMapKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsonMapKey {
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

impl Serialize for JsonMapKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.str)
    }
}

impl Display for JsonMapKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.str)
    }
}

fn flatten_value(prefix_key: &str, value: Value, bucket: u32) -> BTreeMap<JsonMapKey, Value> {
    let mut map = BTreeMap::<JsonMapKey, Value>::new();
    match value {
        Value::Object(obj) => {
            for (key, value) in obj {
                if let Value::Object(inner_obj) = value {
                    let inner_map = flatten_value(&key, Value::Object(inner_obj), bucket);
                    for (inner_key, inner_value) in inner_map {
                        map.insert(
                            JsonMapKey::new(format!("{prefix_key}.{inner_key}"), bucket),
                            inner_value,
                        );
                    }
                } else {
                    map.insert(
                        JsonMapKey::new(format!("{prefix_key}.{key}"), bucket),
                        value,
                    );
                }
            }
        }
        _ => {
            map.insert(JsonMapKey::new(prefix_key, bucket), value);
        }
    }

    map
}

#[cfg(test)]
mod tests {
    use crate::util::json_map::{FlatJsonMap, JsonMapKey};
    use serde_json::json;
    use std::cmp::Ordering;

    macro_rules! key {
        ($str: expr) => {
            JsonMapKey::new($str, 0)
        };
        ($str: expr, $order: expr) => {
            JsonMapKey::new($str, $order)
        };
    }

    #[test]
    fn test_order() {
        assert_eq!(key!("a").cmp(&key!("b")), Ordering::Less);
        assert_eq!(key!("a.b").cmp(&key!("b")), Ordering::Greater);
        assert_eq!(key!("a").cmp(&key!("b.a")), Ordering::Less);
        assert_eq!(key!("a.b.c").cmp(&key!("a.b.c")), Ordering::Equal);
        assert_eq!(key!("a.b.c").cmp(&key!("a.b.c", 1)), Ordering::Less);
    }

    #[test]
    fn flatmap_insert_order() {
        let mut o = FlatJsonMap::default();
        o.insert("a", json!("abc"));
        o.insert("a.b", json!("abc"));
        o.insert("a.b.c", json!("abc"));

        assert_eq!(
            o.to_json().unwrap(),
            json!({ "a": { "b": { "c": "abc" } } })
        );
    }

    #[test]
    fn flatmap_secondary_order() {
        let mut o = FlatJsonMap::default();
        o.insert("a.first.deleted", json!("deleted"));
        o.insert("a.third.firstNested", json!("firstNested"));
        o.insert("a.first", json!("first"));
        o.insert("a.second", json!("second"));
        o.insert("a.third.nested", json!("nested"));

        assert_eq!(
            o.to_json().unwrap(),
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

    #[test]
    fn flatmap_nested() {
        let mut o = FlatJsonMap::default();
        o.insert("array", json!([1, 2]));
        o.insert("array", json!([3, 4]));
        o.insert(
            "object",
            json!({
                "a": "a",
                "b": "b",
                "nested": {
                    "array": [10, 11]
                }
            }),
        );
        o.insert(
            "object",
            json!({
                "b": "c",
                "nested": {
                    "array": [12, 13]
                }
            }),
        );

        assert_eq!(
            o.to_json().unwrap(),
            json!({
                "array": [1, 2, 3, 4],
                "object": {
                    "a": "a",
                    "b": "c",
                    "nested": {
                        "array": [10, 11, 12, 13]
                    }
                }
            })
        );
    }

    #[test]
    fn flatmap_with_capacity() {
        let mut a = FlatJsonMap::new();
        assert_eq!(a.capacity(), 0);
        a.reserve(10);
        assert_eq!(a.capacity(), 10);

        let mut b = FlatJsonMap::with_capacity(10);
        assert_eq!(b.capacity(), 10);
        b.reserve(20);
        assert_eq!(b.capacity(), 20);
    }
}

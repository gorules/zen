use crate::support::test_data_root;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use zen_engine::model::{DecisionContent, GraphContent};

pub struct Fixture {
    pub name: String,
    pub content: GraphContent,
    pub inputs: Vec<Value>,
}

impl Fixture {
    pub fn load_all() -> Vec<Fixture> {
        let root = PathBuf::from(test_data_root());
        let mut paths: Vec<PathBuf> = fs::read_dir(root.join("graphs"))
            .into_iter()
            .flatten()
            .chain(fs::read_dir(&root).into_iter().flatten())
            .chain(fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/compiled")).into_iter().flatten())
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        paths.iter().filter_map(|p| Self::load(p)).collect()
    }

    pub fn load(path: &Path) -> Option<Fixture> {
        let text = fs::read_to_string(path).ok()?;
        let raw: Value = serde_json::from_str(&text).ok()?;
        let content: DecisionContent = serde_json::from_value(raw.clone()).ok()?;
        let DecisionContent::Graph(graph) = content else {
            return None;
        };
        let mut inputs: Vec<Value> = raw
            .get("tests")
            .and_then(Value::as_array)
            .map(|tests| tests.iter().filter_map(|t| t.get("input").cloned()).collect())
            .unwrap_or_default();
        if inputs.is_empty() {
            inputs.push(json!({}));
        }
        Some(Fixture {
            name: path.file_name()?.to_string_lossy().to_string(),
            content: (*graph).clone(),
            inputs,
        })
    }

    pub fn typed_variants(&self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for base in &self.inputs {
            out.push(base.clone());
            let Some(object) = base.as_object() else {
                continue;
            };
            for (key, value) in object {
                let mut with = |replacement: Option<Value>| {
                    let mut copy = object.clone();
                    match replacement {
                        Some(v) => copy.insert(key.clone(), v),
                        None => copy.remove(key),
                    };
                    out.push(Value::Object(copy));
                };
                with(None);
                match value {
                    Value::Number(n) => {
                        for delta in ["1", "-1", "0", "1000000", "0.5"] {
                            let shifted: Option<rust_decimal::Decimal> = n
                                .to_string()
                                .parse::<rust_decimal::Decimal>()
                                .ok()
                                .zip(delta.parse::<rust_decimal::Decimal>().ok())
                                .and_then(|(a, b)| a.checked_add(b));
                            if let Some(v) = shifted.and_then(|d| serde_json::from_str(&d.to_string()).ok()) {
                                with(Some(v));
                            }
                        }
                    }
                    Value::String(s) => {
                        with(Some(json!("")));
                        with(Some(json!(format!("{s}x"))));
                    }
                    Value::Bool(b) => with(Some(json!(!b))),
                    _ => {}
                }
            }
        }
        out.truncate(400);
        out
    }

    pub fn variants(&self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for base in &self.inputs {
            out.push(base.clone());
            let Some(object) = base.as_object() else {
                continue;
            };
            for (key, value) in object {
                let mut with = |replacement: Option<Value>| {
                    let mut copy = object.clone();
                    match replacement {
                        Some(v) => {
                            copy.insert(key.clone(), v);
                        }
                        None => {
                            copy.remove(key);
                        }
                    }
                    out.push(Value::Object(copy));
                };
                with(None);
                with(Some(Value::Null));
                match value {
                    Value::Number(n) => {
                        let text = n.to_string();
                        for delta in ["1", "-1", "0", "1000000", "0.5"] {
                            let shifted: Option<rust_decimal::Decimal> = text
                                .parse::<rust_decimal::Decimal>()
                                .ok()
                                .zip(delta.parse::<rust_decimal::Decimal>().ok())
                                .and_then(|(a, b)| a.checked_add(b));
                            if let Some(v) = shifted.and_then(|d| serde_json::from_str(&d.to_string()).ok()) {
                                with(Some(v));
                            }
                        }
                        with(Some(json!("text")));
                    }
                    Value::String(s) => {
                        with(Some(json!("")));
                        with(Some(json!(format!("{s}x"))));
                        with(Some(json!(42)));
                    }
                    Value::Bool(b) => with(Some(json!(!b))),
                    Value::Array(items) => {
                        with(Some(json!([])));
                        let mut doubled = items.clone();
                        doubled.extend(items.iter().cloned());
                        with(Some(Value::Array(doubled)));
                    }
                    Value::Object(_) => with(Some(json!({}))),
                    Value::Null => with(Some(json!(1))),
                }
            }
        }
        out.truncate(400);
        out
    }
}

pub const NETWORK_BOUND: &[&str] = &["http-function.json"];
pub const TIME_BOUND: &[&str] = &["infinite-function.json", "sleep-function.json"];

use std::hash::Hasher;
use std::sync::Arc;

use ahash::{AHasher, HashMap, HashMapExt, HashSet, HashSetExt};
use serde_json::{Map, Value};

use crate::loader::DynamicLoader;
use crate::policy::raw::BlockDoc;

pub(crate) fn schema_references_dictionary(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key("$dictionary") || map.values().any(schema_references_dictionary)
        }
        Value::Array(items) => items.iter().any(schema_references_dictionary),
        _ => false,
    }
}

pub(crate) async fn load_import_dictionaries(
    loader: &DynamicLoader,
    imports: &[Arc<str>],
) -> Result<HashMap<Arc<str>, Vec<Arc<str>>>, String> {
    let mut out: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
    let mut visited: HashSet<Arc<str>> = HashSet::new();
    let mut queue: Vec<Arc<str>> = imports.to_vec();
    while let Some(key) = queue.pop() {
        if !visited.insert(key.clone()) {
            continue;
        }
        let loaded = loader
            .load(key.as_ref())
            .await
            .map_err(|error| format!("failed to load imported policy '{key}': {error:?}"))?;
        let Some(policy) = loaded.as_policy() else {
            continue;
        };
        let document = policy.0.clone();
        queue.extend(document.imports.iter().cloned());
        for block in &document.blocks {
            if let BlockDoc::Dictionary { data, .. } = block {
                out.entry(data.name.clone()).or_insert_with(|| {
                    data.entries
                        .iter()
                        .map(|entry| entry.value.clone())
                        .collect()
                });
            }
        }
    }
    Ok(out)
}

pub(crate) fn resolve_schema(
    schema: &Value,
    dictionaries: &HashMap<Arc<str>, Vec<Arc<str>>>,
) -> Result<(Value, u64), String> {
    let mut hasher = AHasher::default();
    let resolved = rewrite(schema, dictionaries, &mut hasher)?;
    Ok((resolved, hasher.finish()))
}

fn rewrite(
    value: &Value,
    dictionaries: &HashMap<Arc<str>, Vec<Arc<str>>>,
    hasher: &mut AHasher,
) -> Result<Value, String> {
    match value {
        Value::Object(map) => {
            if let Some(name) = map.get("$dictionary") {
                let Some(name) = name.as_str() else {
                    return Err("$dictionary must be a string dictionary name".to_string());
                };
                let Some(values) = dictionaries.get(name) else {
                    return Err(format!(
                        "unknown dictionary '{name}' in schema: no dictionary with that name is reachable through the graph's imports"
                    ));
                };
                hasher.write(name.as_bytes());
                let mut next = Map::new();
                for (key, entry) in map {
                    if key == "$dictionary" || key == "type" || key == "enum" {
                        continue;
                    }
                    next.insert(key.clone(), entry.clone());
                }
                next.insert("type".to_string(), Value::String("string".to_string()));
                next.insert(
                    "enum".to_string(),
                    Value::Array(
                        values
                            .iter()
                            .map(|entry| {
                                hasher.write(entry.as_bytes());
                                Value::String(entry.to_string())
                            })
                            .collect(),
                    ),
                );
                return Ok(Value::Object(next));
            }
            let mut next = Map::new();
            for (key, entry) in map {
                next.insert(key.clone(), rewrite(entry, dictionaries, hasher)?);
            }
            Ok(Value::Object(next))
        }
        Value::Array(items) => Ok(Value::Array(
            items
                .iter()
                .map(|entry| rewrite(entry, dictionaries, hasher))
                .collect::<Result<_, _>>()?,
        )),
        _ => Ok(value.clone()),
    }
}

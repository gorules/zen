use super::literal::{Kind, Lit};
use serde_json::{json, Map, Value};

const KINDS: &[(&str, &str)] = &[
    ("input", "inputNode"),
    ("output", "outputNode"),
    ("expression", "expressionNode"),
    ("table", "decisionTableNode"),
    ("switch", "switchNode"),
    ("function", "functionNode"),
    ("functionV1", "functionNode"),
    ("decision", "decisionNode"),
    ("custom", "customNode"),
];

pub struct Graph;

impl Graph {
    pub fn expand(lit: &Lit) -> Result<Value, String> {
        if let Some(raw) = lit.get("raw") {
            return Ok(raw.json());
        }
        let specs = match lit.get("nodes") {
            Some(nodes) => nodes.list().ok_or("`nodes` must be a list")?,
            None => &[],
        };
        let mut nodes: Vec<(String, &str, Value)> = Vec::new();
        for spec in specs {
            let (name, kind, node) = Self::node(spec)?;
            if nodes.iter().any(|(n, _, _)| *n == name) {
                return Err(format!("duplicate node name {name:?}"));
            }
            nodes.push((name, kind, node));
        }
        if !nodes.iter().any(|(_, k, _)| *k == "input") {
            nodes.insert(0, ("input".into(), "input", json!({"id": "input", "name": "input", "type": "inputNode", "content": {}})));
        }
        if !nodes.iter().any(|(_, k, _)| *k == "output") {
            nodes.push(("output".into(), "output", json!({"id": "output", "name": "output", "type": "outputNode", "content": {}})));
        }
        let id = |name: &str| -> Result<String, String> {
            nodes
                .iter()
                .find(|(n, _, _)| n == name)
                .and_then(|(_, _, node)| node.get("id").and_then(Value::as_str).map(str::to_string))
                .ok_or_else(|| format!("edge names unknown node {name:?}"))
        };
        let mut edges: Vec<Value> = Vec::new();
        let mut push = |from: &str, to: &str| -> Result<(), String> {
            let (source, handle) = match from.rsplit_once(':') {
                Some((source, handle)) if handle.chars().all(|c| c.is_ascii_digit()) => {
                    (source, Some(format!("{}:{handle}", id(source)?)))
                }
                _ => (from, None),
            };
            edges.push(json!({
                "id": format!("e{}", edges.len()),
                "sourceId": id(source)?,
                "targetId": id(to)?,
                "sourceHandle": handle,
            }));
            Ok(())
        };
        match lit.get("edges") {
            Some(list) => {
                for chain in list.list().ok_or("`edges` must be a list")? {
                    let text = chain.text().ok_or("edge must be a string like \"a -> b\"")?;
                    let hops: Vec<&str> = text.split("->").map(str::trim).collect();
                    for pair in hops.windows(2) {
                        push(pair[0], pair[1])?;
                    }
                }
            }
            None => {
                let order: Vec<&str> = nodes
                    .iter()
                    .filter(|(_, k, _)| *k == "input")
                    .chain(nodes.iter().filter(|(_, k, _)| *k != "input" && *k != "output"))
                    .chain(nodes.iter().filter(|(_, k, _)| *k == "output"))
                    .map(|(n, _, _)| n.as_str())
                    .collect();
                if nodes.iter().any(|(_, k, _)| *k == "switch") {
                    return Err("a graph with a switch needs explicit `edges`".into());
                }
                for pair in order.windows(2) {
                    push(pair[0], pair[1])?;
                }
            }
        }
        Ok(json!({
            "nodes": nodes.into_iter().map(|(_, _, node)| node).collect::<Vec<_>>(),
            "edges": edges,
        }))
    }

    fn node(spec: &Lit) -> Result<(String, &'static str, Value), String> {
        let entries = spec.map().ok_or("node must be an object")?;
        let found: Vec<(&str, &str, &Lit)> = KINDS
            .iter()
            .filter_map(|(short, wire)| spec.get(short).map(|v| (*short, *wire, v)))
            .collect();
        let [(short, wire, name)] = found.as_slice() else {
            return Err(format!(
                "node needs exactly one of {:?}, got {:?}",
                KINDS.iter().map(|k| k.0).collect::<Vec<_>>(),
                entries.iter().map(|e| e.0.as_str()).collect::<Vec<_>>()
            ));
        };
        let name = name.text().ok_or("node name must be a string")?.to_string();
        let id = spec.get("id").and_then(Lit::text).unwrap_or(&name).to_string();
        let mut content = Map::new();
        match *short {
            "input" | "output" => {
                if let Some(schema) = spec.get("schema") {
                    let text = match schema.text() {
                        Some(text) => text.to_string(),
                        None => schema.json().to_string(),
                    };
                    content.insert("schema".into(), Value::String(text));
                }
            }
            "expression" => {
                let fields = spec.get("fields").ok_or("expression node needs `fields`")?;
                let pairs: Vec<(String, String)> = match &fields.kind {
                    Kind::Map(entries) => entries
                        .iter()
                        .map(|(k, v)| v.text().map(|v| (k.clone(), v.to_string())))
                        .collect::<Option<_>>()
                        .ok_or("expression values must be strings")?,
                    Kind::List(items) => items
                        .iter()
                        .map(|pair| match pair.list() {
                            Some([k, v]) => k.text().zip(v.text()).map(|(k, v)| (k.to_string(), v.to_string())),
                            _ => None,
                        })
                        .collect::<Option<_>>()
                        .ok_or("expression `fields` list items must be [key, value] strings")?,
                    _ => return Err("expression `fields` must be an object or a list of pairs".into()),
                };
                let expressions: Vec<Value> = pairs
                    .into_iter()
                    .enumerate()
                    .map(|(i, (key, value))| json!({"id": format!("x{i}"), "key": key, "value": value}))
                    .collect();
                content.insert("expressions".into(), Value::Array(expressions));
            }
            "table" => Self::table(spec, &mut content)?,
            "switch" => {
                let when = spec.get("when").and_then(Lit::list).ok_or("switch node needs `when`")?;
                let statements: Vec<Value> = when
                    .iter()
                    .enumerate()
                    .map(|(i, c)| c.text().map(|c| json!({"id": format!("{id}:{i}"), "condition": c})))
                    .collect::<Option<_>>()
                    .ok_or("switch conditions must be strings")?;
                content.insert("statements".into(), Value::Array(statements));
                content.insert("hitPolicy".into(), Value::String(spec.get("hit").and_then(Lit::text).unwrap_or("first").into()));
            }
            "function" => {
                let source = spec.get("source").and_then(Lit::text).ok_or("function node needs `source`")?;
                content.insert("source".into(), Value::String(source.into()));
            }
            "functionV1" => {
                let source = spec.get("source").and_then(Lit::text).ok_or("functionV1 node needs `source`")?;
                return Ok((name.clone(), "function", json!({"id": id, "name": name, "type": wire, "content": source})));
            }
            "decision" => {
                let key = spec.get("key").and_then(Lit::text).ok_or("decision node needs `key`")?;
                content.insert("key".into(), Value::String(key.into()));
            }
            "custom" => {
                content.insert("kind".into(), Value::String(spec.get("kind").and_then(Lit::text).unwrap_or_default().into()));
                content.insert("config".into(), spec.get("config").map_or(json!({}), Lit::json));
            }
            _ => {}
        }
        if matches!(*short, "expression" | "table" | "decision") {
            for key in ["inputField", "outputPath", "executionMode"] {
                if let Some(value) = spec.get(key) {
                    content.insert(key.into(), value.json());
                }
            }
            content.insert("passThrough".into(), Value::Bool(spec.flag("passThrough")));
        }
        Ok((name.clone(), short, json!({"id": id, "name": name, "type": wire, "content": content})))
    }

    fn table(spec: &Lit, content: &mut Map<String, Value>) -> Result<(), String> {
        let column = |lit: &Lit, prefix: &str, i: usize, output: bool| -> Result<Value, String> {
            let mut out = json!({"id": format!("{prefix}{i}"), "name": format!("{prefix}{i}")});
            match &lit.kind {
                Kind::Null => {}
                Kind::Text(field) => {
                    out["field"] = Value::String(field.clone());
                }
                Kind::Map(_) => {
                    for key in ["field", "name", "type"] {
                        if let Some(v) = lit.get(key) {
                            out[key] = v.json();
                        }
                    }
                }
                _ => return Err("table column must be a string, null or object".into()),
            }
            if output && out.get("field").is_none() {
                return Err("output column needs a field".into());
            }
            Ok(out)
        };
        let inputs: Vec<Value> = spec
            .get("inputs")
            .and_then(Lit::list)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, c)| column(c, "i", i, false))
            .collect::<Result<_, _>>()?;
        let outputs: Vec<Value> = spec
            .get("outputs")
            .and_then(Lit::list)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, c)| column(c, "o", i, true))
            .collect::<Result<_, _>>()?;
        let ids: Vec<String> = inputs
            .iter()
            .chain(&outputs)
            .filter_map(|c| c.get("id").and_then(Value::as_str).map(str::to_string))
            .collect();
        let rules: Vec<Value> = spec
            .get("rules")
            .and_then(Lit::list)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(r, row)| {
                let cells = row.list().ok_or("rule must be a list of cells")?;
                if cells.len() != ids.len() {
                    return Err(format!("rule {r} has {} cells, table has {} columns", cells.len(), ids.len()));
                }
                let mut rule = Map::new();
                rule.insert("_id".into(), Value::String(format!("r{r}")));
                for (id, cell) in ids.iter().zip(cells) {
                    let text = match &cell.kind {
                        Kind::Null => String::new(),
                        Kind::Text(t) => t.clone(),
                        _ => return Err(format!("rule {r}: cells are expression strings, got {}", cell.source())),
                    };
                    rule.insert(id.clone(), Value::String(text));
                }
                Ok(Value::Object(rule))
            })
            .collect::<Result<_, String>>()?;
        content.insert("inputs".into(), Value::Array(inputs));
        content.insert("outputs".into(), Value::Array(outputs));
        content.insert("rules".into(), Value::Array(rules));
        content.insert("hitPolicy".into(), Value::String(spec.get("hit").and_then(Lit::text).unwrap_or("first").into()));
        Ok(())
    }
}

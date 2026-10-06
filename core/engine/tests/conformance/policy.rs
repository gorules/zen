use super::literal::{Kind, Lit};
use serde_json::{json, Map, Value};

const PRIMITIVES: &[&str] = &["string", "number", "boolean", "date"];

pub struct Policy;

impl Policy {
    pub fn expand(lit: &Lit) -> Result<Value, String> {
        if let Some(raw) = lit.get("raw") {
            return Ok(raw.json());
        }
        let mut blocks: Vec<Value> = Vec::new();
        for (key, scope) in [("models", "entity"), ("globals", "global")] {
            for (name, props) in lit.get(key).and_then(Lit::map).unwrap_or_default() {
                let properties: Vec<Value> = props
                    .map()
                    .ok_or_else(|| format!("model {name} must be an object of property types"))?
                    .iter()
                    .map(|(prop, ty)| Self::property(name, prop, ty))
                    .collect::<Result<_, _>>()?;
                blocks.push(json!({
                    "id": format!("model:{name}"),
                    "type": "dataModel",
                    "props": {"data": {"name": name, "scope": scope, "properties": properties}},
                }));
            }
        }
        for (name, entries) in lit.get("dictionaries").and_then(Lit::map).unwrap_or_default() {
            let entries: Vec<Value> = entries
                .list()
                .ok_or_else(|| format!("dictionary {name} must be a list"))?
                .iter()
                .enumerate()
                .map(|(i, entry)| match &entry.kind {
                    Kind::Text(value) => Ok(json!({"id": format!("{name}:{i}"), "value": value, "label": value})),
                    Kind::List(pair) => match pair.as_slice() {
                        [value, label] => Ok(json!({"id": format!("{name}:{i}"), "value": value.json(), "label": label.json()})),
                        _ => Err(format!("dictionary {name} entry {i} must be \"value\" or [value, label]")),
                    },
                    _ => Err(format!("dictionary {name} entry {i} must be \"value\" or [value, label]")),
                })
                .collect::<Result<_, String>>()?;
            blocks.push(json!({
                "id": format!("dictionary:{name}"),
                "type": "dictionary",
                "props": {"data": {"name": name, "entries": entries}},
            }));
        }
        for (i, block) in lit.get("blocks").and_then(Lit::list).unwrap_or_default().iter().enumerate() {
            blocks.push(Self::block(i, block)?);
        }
        let imports: Vec<Value> = lit.get("imports").map(|l| l.json()).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        Ok(json!({"imports": imports, "blocks": blocks}))
    }

    fn property(model: &str, name: &str, ty: &Lit) -> Result<Value, String> {
        let text = ty.text().ok_or_else(|| format!("{model}.{name}: type must be a string like \"number[]?\""))?;
        let (text, optional) = match text.strip_suffix('?') {
            Some(rest) => (rest, true),
            None => (text, false),
        };
        let (text, array) = match text.strip_suffix("[]") {
            Some(rest) => (rest, true),
            None => (text, false),
        };
        let mut out = json!({"id": format!("{model}.{name}"), "name": name, "array": array, "optional": optional});
        if let Some(values) = text.strip_prefix("string(").and_then(|r| r.strip_suffix(')')) {
            out["type"] = json!("string");
            out["enum"] = json!(values.split('|').map(str::trim).collect::<Vec<_>>());
        } else if PRIMITIVES.contains(&text) {
            out["type"] = json!(text);
        } else if let Some(target) = text.strip_prefix('&') {
            out["type"] = json!("reference");
            out["target"] = json!(target);
        } else {
            out["type"] = json!("relationship");
            out["target"] = json!(text);
        }
        Ok(out)
    }

    fn block(i: usize, spec: &Lit) -> Result<Value, String> {
        let text = |key: &str| spec.get(key).and_then(Lit::text).map(str::to_string);
        let id = text("id").or_else(|| text("table")).unwrap_or_else(|| format!("b{i}"));
        let (kind, data) = if let Some(key) = text("expression") {
            let value = text("value").ok_or("expression block needs `value`")?;
            ("expression", json!({"key": key, "value": value}))
        } else if let Some(key) = text("match") {
            let arms: Vec<Value> = spec
                .get("arms")
                .and_then(Lit::list)
                .ok_or("match block needs `arms`")?
                .iter()
                .enumerate()
                .map(|(a, arm)| match arm.list() {
                    Some([condition, value]) => condition
                        .text()
                        .zip(value.text())
                        .map(|(c, v)| json!({"id": format!("{id}:{a}"), "condition": c, "value": v}))
                        .ok_or_else(|| "match arm cells must be strings".to_string()),
                    _ => Err("match arm must be [condition, value]".to_string()),
                })
                .collect::<Result<_, _>>()?;
            ("match", json!({"key": key, "arms": arms}))
        } else if let Some(output) = text("assertion") {
            let conditions: Vec<Value> = spec
                .get("conditions")
                .and_then(Lit::list)
                .ok_or("assertion block needs `conditions`")?
                .iter()
                .enumerate()
                .map(|(c, cond)| {
                    let (operator, expression, depth) = match &cond.kind {
                        Kind::Text(e) => ("and".to_string(), e.clone(), 0u64),
                        Kind::List(parts) => match parts.as_slice() {
                            [op, e] => (op.text().unwrap_or("and").to_string(), e.text().unwrap_or_default().to_string(), 0),
                            [op, e, depth] => (
                                op.text().unwrap_or("and").to_string(),
                                e.text().unwrap_or_default().to_string(),
                                depth.json().as_u64().unwrap_or(0),
                            ),
                            _ => return Err("assertion condition must be \"expr\" or [operator, expr, depth?]".to_string()),
                        },
                        _ => return Err("assertion condition must be \"expr\" or [operator, expr, depth?]".to_string()),
                    };
                    Ok(json!({"id": format!("{id}:{c}"), "expression": expression, "operator": operator, "depth": depth}))
                })
                .collect::<Result<_, _>>()?;
            ("assertion", json!({"output": output, "conditions": conditions}))
        } else if spec.get("table").is_some() {
            ("decisionTable", Self::table(spec)?)
        } else {
            return Err(format!(
                "block needs one of expression/match/assertion/table, got {:?}",
                spec.map().unwrap_or_default().iter().map(|e| e.0.as_str()).collect::<Vec<_>>()
            ));
        };
        Ok(json!({"id": id, "type": kind, "props": {"data": data}}))
    }

    fn table(spec: &Lit) -> Result<Value, String> {
        let columns = |key: &str, prefix: &str| -> Result<Vec<Value>, String> {
            spec.get(key)
                .and_then(Lit::list)
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(i, c)| match &c.kind {
                    Kind::Null => Ok(json!({"id": format!("{prefix}{i}"), "name": ""})),
                    Kind::Text(field) => Ok(json!({"id": format!("{prefix}{i}"), "name": "", "field": field})),
                    Kind::Map(_) => {
                        let mut out = c.json();
                        out["id"] = json!(format!("{prefix}{i}"));
                        Ok(out)
                    }
                    _ => Err("table column must be a string, null or object".to_string()),
                })
                .collect()
        };
        let inputs = columns("inputs", "i")?;
        let outputs = columns("outputs", "o")?;
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
                rule.insert("_id".into(), json!(format!("r{r}")));
                for (id, cell) in ids.iter().zip(cells) {
                    let text = match &cell.kind {
                        Kind::Null => String::new(),
                        Kind::Text(t) => t.clone(),
                        _ => return Err(format!("rule {r}: cells are expression strings, got {}", cell.source())),
                    };
                    rule.insert(id.clone(), json!(text));
                }
                Ok(Value::Object(rule))
            })
            .collect::<Result<_, String>>()?;
        let hit = spec.get("hit").and_then(Lit::text).unwrap_or("first");
        Ok(json!({"hitPolicy": hit, "inputs": inputs, "outputs": outputs, "rules": rules}))
    }
}

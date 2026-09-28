use serde_json::{Map, Value};
use zen_expression::DateValue;
use zen_types::variable::Variable;

pub(crate) struct DeclaredDates;

impl DeclaredDates {
    const DATE_KEYS: [&str; 10] = [
        "type",
        "format",
        "description",
        "title",
        "examples",
        "default",
        "$comment",
        "readOnly",
        "writeOnly",
        "deprecated",
    ];

    pub(crate) fn declared(schema: &Value) -> bool {
        schema.as_object().is_some_and(Self::declared_map)
    }

    pub(crate) fn declared_map(schema: &Map<String, Value>) -> bool {
        let format = schema.get("format").and_then(Value::as_str);
        if matches!(format, Some("date" | "date-time")) && Self::only_keys(schema, &Self::DATE_KEYS)
        {
            return Self::string_type(schema.get("type"));
        }

        let Some(variants) = schema.get("anyOf").and_then(Value::as_array) else {
            return false;
        };
        let is_null = |v: &Value| {
            v.as_object().is_some_and(|o| {
                o.len() == 1 && o.get("type").and_then(Value::as_str) == Some("null")
            })
        };
        Self::only_keys(schema, &["anyOf", "description"])
            && variants.len() == 2
            && variants.iter().any(is_null)
            && variants.iter().any(|v| !is_null(v) && Self::declared(v))
    }

    pub(crate) fn prepare(value: &Variable, schema: Option<&Value>) -> Option<Variable> {
        if schema.is_some_and(Self::declared) {
            return match value {
                Variable::String(text) => DateValue::from_text(text),
                _ => None,
            };
        }
        let object = schema.and_then(Value::as_object);
        match value {
            Variable::Dynamic(_) => DateValue::source_text(value),
            Variable::Object(_) => {
                let properties = object
                    .and_then(|o| Self::structure(o, "properties"))
                    .and_then(Value::as_object);
                Self::rewrite_fields(value, |key, child| {
                    Self::prepare(child, properties.and_then(|p| p.get(key)))
                })
            }
            Variable::Array(_) => {
                let items = object.and_then(|o| Self::structure(o, "items"));
                Self::rewrite_items(value, |item| Self::prepare(item, items))
            }
            _ => None,
        }
    }

    fn structure<'s>(schema: &'s Map<String, Value>, key: &str) -> Option<&'s Value> {
        schema.get(key).or_else(|| {
            ["anyOf", "oneOf", "allOf"]
                .iter()
                .filter_map(|keyword| schema.get(*keyword)?.as_array())
                .flatten()
                .find_map(|variant| variant.get(key))
        })
    }

    pub(crate) fn rewrite_fields(
        value: &Variable,
        rewrite: impl Fn(&str, &Variable) -> Option<Variable>,
    ) -> Option<Variable> {
        let object = value.as_object()?;
        let changed: Vec<(String, Variable)> = object
            .borrow()
            .iter()
            .filter_map(|(key, child)| {
                let key: &str = key.as_ref();
                rewrite(key, child).map(|next| (key.to_string(), next))
            })
            .collect();
        if changed.is_empty() {
            return None;
        }
        let mut next = object.borrow().clone();
        for (key, rewritten) in changed {
            next.insert_str(&key, rewritten);
        }
        Some(Variable::from_object(next))
    }

    pub(crate) fn rewrite_items(
        value: &Variable,
        rewrite: impl Fn(&Variable) -> Option<Variable>,
    ) -> Option<Variable> {
        let array = value.as_array()?;
        let array = array.borrow();
        let rewritten: Vec<Option<Variable>> = array.iter().map(&rewrite).collect();
        if rewritten.iter().all(Option::is_none) {
            return None;
        }
        Some(Variable::from_array(
            array
                .iter()
                .zip(rewritten)
                .map(|(item, next)| next.unwrap_or_else(|| item.clone()))
                .collect(),
        ))
    }

    fn string_type(value: Option<&Value>) -> bool {
        match value {
            Some(Value::String(t)) => t == "string",
            Some(Value::Array(types)) if types.len() == 2 => {
                let has = |name: &str| types.iter().any(|t| t.as_str() == Some(name));
                has("string") && has("null")
            }
            _ => false,
        }
    }

    fn only_keys(schema: &Map<String, Value>, allowed: &[&str]) -> bool {
        schema.keys().all(|key| allowed.contains(&key.as_str()))
    }
}

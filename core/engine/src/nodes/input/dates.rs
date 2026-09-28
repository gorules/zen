use serde_json::{Map, Value};
use zen_expression::DateValue;
use zen_types::variable::Variable;

pub(crate) struct DeclaredDates;

impl DeclaredDates {
    pub(crate) fn declared(schema: &Value) -> Option<bool> {
        Self::declared_map(schema.as_object()?)
    }

    pub(crate) fn declared_map(schema: &Map<String, Value>) -> Option<bool> {
        let format = schema.get("format").and_then(Value::as_str);
        if matches!(format, Some("date" | "date-time"))
            && Self::only_keys(
                schema,
                &[
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
                ],
            )
        {
            return Self::string_type(schema.get("type"));
        }

        let variants = schema.get("anyOf")?.as_array()?;
        if !Self::only_keys(schema, &["anyOf", "description"]) || variants.len() != 2 {
            return None;
        }
        let is_null = |v: &Value| {
            v.as_object().is_some_and(|o| {
                o.len() == 1 && o.get("type").and_then(Value::as_str) == Some("null")
            })
        };
        let other = variants.iter().find(|v| !is_null(v))?;
        variants.iter().any(is_null).then_some(())?;
        Self::declared(other).map(|_| true)
    }

    pub(crate) fn convert(value: &Variable, schema: &Value) -> Option<Variable> {
        if Self::declared(schema).is_some() {
            return match value {
                Variable::String(text) => DateValue::from_text(text),
                _ => None,
            };
        }
        let object = schema.as_object()?;
        if DateValue::is(value) && Self::string_type(object.get("type")).is_some() {
            return Some(Variable::String(value.to_string().into()));
        }

        if let (Some(properties), Some(map)) = (
            object.get("properties").and_then(Value::as_object),
            value.as_object(),
        ) {
            let changed: Vec<(String, Variable)> = {
                let map = map.borrow();
                properties
                    .iter()
                    .filter_map(|(key, property)| {
                        let child = map.get_str(key)?;
                        Self::convert(child, property).map(|converted| (key.clone(), converted))
                    })
                    .collect()
            };
            if changed.is_empty() {
                return None;
            }
            let mut next = map.borrow().clone();
            for (key, converted) in changed {
                next.insert_str(&key, converted);
            }
            return Some(Variable::from_object(next));
        }

        if let (Some(items), Some(array)) = (object.get("items"), value.as_array()) {
            let array = array.borrow();
            let converted: Vec<Option<Variable>> = array
                .iter()
                .map(|item| Self::convert(item, items))
                .collect();
            if converted.iter().all(Option::is_none) {
                return None;
            }
            return Some(Variable::from_array(
                array
                    .iter()
                    .zip(converted)
                    .map(|(item, converted)| converted.unwrap_or_else(|| item.clone()))
                    .collect(),
            ));
        }

        ["anyOf", "oneOf", "allOf"]
            .iter()
            .filter_map(|keyword| object.get(*keyword)?.as_array())
            .flatten()
            .find_map(|variant| Self::convert(value, variant))
    }

    fn string_type(value: Option<&Value>) -> Option<bool> {
        match value? {
            Value::String(t) if t == "string" => Some(false),
            Value::Array(types) if types.len() == 2 => {
                let has = |name: &str| types.iter().any(|t| t.as_str() == Some(name));
                (has("string") && has("null")).then_some(true)
            }
            _ => None,
        }
    }

    fn only_keys(schema: &Map<String, Value>, allowed: &[&str]) -> bool {
        schema.keys().all(|key| allowed.contains(&key.as_str()))
    }
}

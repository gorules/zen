use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt};
use serde_json::{Map, Value};
use zen_expression::variable::VariableType;

pub(crate) type SchemaDictionaries = HashMap<Arc<str>, VariableType>;

pub(crate) struct SchemaType;

impl SchemaType {
    pub(crate) fn variable_type_with(
        schema: &Value,
        dictionaries: &SchemaDictionaries,
    ) -> VariableType {
        Self::resolve::<false>(schema, dictionaries)
    }

    pub(crate) fn hint_type_with(
        schema: &Value,
        dictionaries: &SchemaDictionaries,
    ) -> VariableType {
        Self::resolve::<true>(schema, dictionaries)
    }

    pub(crate) fn is_date_path(schema: &Value, path: &str) -> bool {
        if let Some(cases) = schema
            .get("anyOf")
            .or_else(|| schema.get("oneOf"))
            .and_then(Value::as_array)
        {
            let mut known = cases
                .iter()
                .filter(|case| case.get("type").and_then(Value::as_str) != Some("null"));
            return known
                .next()
                .is_some_and(|case| Self::is_date_path(case, path))
                && known.all(|case| Self::is_date_path(case, path));
        }
        if let Some(items) = schema.get("items") {
            return Self::is_date_path(items, path);
        }
        if path.is_empty() {
            return matches!(
                schema.get("format").and_then(Value::as_str),
                Some("date" | "date-time")
            );
        }
        let (field, rest) = path.split_once('.').unwrap_or((path, ""));
        schema
            .get("properties")
            .and_then(|props| props.get(field))
            .is_some_and(|child| Self::is_date_path(child, rest))
    }

    fn resolve<const DATE_HINTS: bool>(
        schema: &Value,
        dictionaries: &SchemaDictionaries,
    ) -> VariableType {
        let Some(object) = schema.as_object() else {
            return VariableType::Any;
        };

        if let Some(name) = object.get("$dictionary").and_then(Value::as_str) {
            return dictionaries
                .get(name)
                .map(VariableType::shallow_clone)
                .unwrap_or(VariableType::Any);
        }

        if let Some(cases) = object
            .get("anyOf")
            .or_else(|| object.get("oneOf"))
            .and_then(Value::as_array)
        {
            return cases
                .iter()
                .map(|case| Self::resolve::<DATE_HINTS>(case, dictionaries))
                .reduce(|acc, t| acc.merge(&t))
                .unwrap_or(VariableType::Any);
        }

        if let Some(values) = object.get("enum").and_then(Value::as_array) {
            let strings: Vec<Rc<str>> = values
                .iter()
                .filter_map(Value::as_str)
                .map(Rc::from)
                .collect();
            if strings.len() == values.len() && !strings.is_empty() {
                return VariableType::Enum(None, strings);
            }
        }

        match object.get("type") {
            Some(Value::String(kind)) => Self::typed::<DATE_HINTS>(object, kind, dictionaries),
            Some(Value::Array(kinds)) => kinds
                .iter()
                .filter_map(Value::as_str)
                .map(|kind| Self::typed::<DATE_HINTS>(object, kind, dictionaries))
                .reduce(|acc, t| acc.merge(&t))
                .unwrap_or(VariableType::Any),
            _ => VariableType::Any,
        }
    }

    pub(crate) fn inline_enum_paths(schema: &Value) -> Vec<String> {
        let mut out = Vec::new();
        Self::collect_inline_enums(schema, String::new(), &mut out);
        out
    }

    fn collect_inline_enums(schema: &Value, path: String, out: &mut Vec<String>) {
        let Some(object) = schema.as_object() else {
            return;
        };
        if object.get("$dictionary").is_none() && !path.is_empty() {
            if let Some(values) = object.get("enum").and_then(Value::as_array) {
                let strings = values.iter().filter(|value| value.is_string()).count();
                if strings == values.len() && values.len() >= 2 {
                    out.push(path.clone());
                }
            }
        }
        if let Some(items) = object.get("items") {
            let item_path = if path.is_empty() {
                "[]".to_string()
            } else {
                format!("{path}[]")
            };
            Self::collect_inline_enums(items, item_path, out);
        }
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            for (name, prop_schema) in properties {
                let child_path = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                Self::collect_inline_enums(prop_schema, child_path, out);
            }
        }
    }

    pub(crate) fn dictionary_names(schema: &Value, out: &mut Vec<Arc<str>>) {
        match schema {
            Value::Object(map) => {
                if let Some(name) = map.get("$dictionary").and_then(Value::as_str) {
                    out.push(Arc::from(name));
                }
                for entry in map.values() {
                    Self::dictionary_names(entry, out);
                }
            }
            Value::Array(items) => {
                for entry in items {
                    Self::dictionary_names(entry, out);
                }
            }
            _ => {}
        }
    }

    fn typed<const DATE_HINTS: bool>(
        object: &Map<String, Value>,
        kind: &str,
        dictionaries: &SchemaDictionaries,
    ) -> VariableType {
        match kind {
            "object" => Self::object_type::<DATE_HINTS>(object, dictionaries),
            "array" => VariableType::Array(Rc::new(
                object
                    .get("items")
                    .map(|items| Self::resolve::<DATE_HINTS>(items, dictionaries))
                    .unwrap_or(VariableType::Any),
            )),
            "string" => match object.get("format").and_then(Value::as_str) {
                Some("date" | "date-time") if DATE_HINTS => VariableType::Date,
                _ => VariableType::String,
            },
            "number" | "integer" => VariableType::Number,
            "boolean" => VariableType::Bool,
            "null" => VariableType::Null,
            _ => VariableType::Any,
        }
    }

    fn object_type<const DATE_HINTS: bool>(
        object: &Map<String, Value>,
        dictionaries: &SchemaDictionaries,
    ) -> VariableType {
        let Some(properties) = object.get("properties").and_then(Value::as_object) else {
            return VariableType::Any;
        };
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        let mut fields: HashMap<Rc<str>, VariableType> = HashMap::with_capacity(properties.len());
        for (name, prop_schema) in properties {
            let mut resolved = Self::resolve::<DATE_HINTS>(prop_schema, dictionaries);
            if !required.contains(&name.as_str()) {
                resolved = super::wrap_optional(resolved);
            }
            fields.insert(Rc::from(name.as_str()), resolved);
        }
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }
}

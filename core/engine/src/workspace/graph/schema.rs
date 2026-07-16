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
                .map(|case| Self::variable_type_with(case, dictionaries))
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
            Some(Value::String(kind)) => Self::typed(object, kind, dictionaries),
            Some(Value::Array(kinds)) => kinds
                .iter()
                .filter_map(Value::as_str)
                .map(|kind| Self::typed(object, kind, dictionaries))
                .reduce(|acc, t| acc.merge(&t))
                .unwrap_or(VariableType::Any),
            _ => VariableType::Any,
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

    fn typed(
        object: &Map<String, Value>,
        kind: &str,
        dictionaries: &SchemaDictionaries,
    ) -> VariableType {
        match kind {
            "object" => Self::object_type(object, dictionaries),
            "array" => VariableType::Array(Rc::new(
                object
                    .get("items")
                    .map(|items| Self::variable_type_with(items, dictionaries))
                    .unwrap_or(VariableType::Any),
            )),
            "string" => match object.get("format").and_then(Value::as_str) {
                Some("date" | "date-time") => VariableType::Date,
                _ => VariableType::String,
            },
            "number" | "integer" => VariableType::Number,
            "boolean" => VariableType::Bool,
            "null" => VariableType::Null,
            _ => VariableType::Any,
        }
    }

    fn object_type(object: &Map<String, Value>, dictionaries: &SchemaDictionaries) -> VariableType {
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
            let mut resolved = Self::variable_type_with(prop_schema, dictionaries);
            if !required.contains(&name.as_str()) {
                resolved = super::wrap_optional(resolved);
            }
            fields.insert(Rc::from(name.as_str()), resolved);
        }
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }
}

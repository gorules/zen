use std::sync::Arc;

use serde_json::{Map, Value};
use zen_expression::variable::VariableType;

use crate::workspace::db::Db;
use crate::workspace::types::ScopeRequest;

impl Db {
    pub fn input_skeleton(&self, req: &ScopeRequest) -> Value {
        let mut builder = SkeletonBuilder::new();
        for input in self.inputs(req) {
            builder.insert(&input.path, &input.resolved_type);
        }
        builder.into_value()
    }
}

struct SkeletonBuilder {
    root: Map<String, Value>,
}

impl SkeletonBuilder {
    fn new() -> Self {
        Self { root: Map::new() }
    }

    fn into_value(self) -> Value {
        Value::Object(self.root)
    }

    fn insert(&mut self, path: &Arc<str>, ty: &VariableType) {
        let value = Self::default_for(ty);
        let mut segments = path.split('.').peekable();
        let mut current = &mut self.root;
        while let Some(seg) = segments.next() {
            if segments.peek().is_none() {
                current.insert(seg.to_string(), value);
                return;
            }
            let entry = current
                .entry(seg.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            current = entry.as_object_mut().expect("just ensured object");
        }
    }

    fn default_for(ty: &VariableType) -> Value {
        match ty {
            VariableType::String | VariableType::Date | VariableType::Interval => {
                Value::String(String::new())
            }
            VariableType::Number => Value::Number(0u64.into()),
            VariableType::Bool => Value::Bool(false),
            VariableType::Null | VariableType::Any => Value::Null,
            VariableType::Nullable(_) => Value::Null,
            VariableType::Array(inner) => Value::Array(vec![Self::default_for(inner)]),
            VariableType::Const(c) => Value::String(c.to_string()),
            VariableType::Enum(_, values) => values
                .first()
                .map(|v| Value::String(v.to_string()))
                .unwrap_or(Value::Null),
            VariableType::Object(obj) => {
                let mut out = Map::new();
                for (k, v) in obj.borrow().iter() {
                    out.insert(k.to_string(), Self::default_for(v));
                }
                Value::Object(out)
            }
        }
    }
}

use crate::variable::types::VariableType;
use serde_json::Value;
use std::borrow::Cow;
use std::ops::Deref;
use std::rc::Rc;

impl<'a> From<Cow<'a, Value>> for VariableType {
    fn from(value: Cow<'a, Value>) -> Self {
        match value.deref() {
            Value::Null => VariableType::Null,
            Value::Bool(_) => VariableType::Bool,
            Value::Number(_) => VariableType::Number,
            Value::String(_) => VariableType::String,
            Value::Array(_) => {
                let Value::Array(arr) = value.into_owned() else {
                    panic!("unexpected type of value, expected array");
                };

                VariableType::from(arr)
            }
            Value::Object(_) => {
                let Value::Object(obj) = value.into_owned() else {
                    panic!("unexpected type of value, expected object");
                };

                VariableType::Object(
                    obj.into_iter()
                        .map(|(k, v)| (k, Rc::new(v.into())))
                        .collect(),
                )
            }
        }
    }
}

impl From<Value> for VariableType {
    fn from(value: Value) -> Self {
        VariableType::from(Cow::Owned(value)).into()
    }
}

impl From<&Value> for VariableType {
    fn from(value: &Value) -> Self {
        VariableType::from(Cow::Borrowed(value)).into()
    }
}

impl From<Vec<Value>> for VariableType {
    fn from(arr: Vec<Value>) -> Self {
        if arr.len() == 0 {
            return VariableType::Array(Rc::new(VariableType::Any));
        }

        let result_type = arr
            .into_iter()
            .fold(None, |acc: Option<VariableType>, b| match acc {
                Some(a) => Some(a.merge(&VariableType::from(b))),
                None => Some(VariableType::from(b)),
            });

        VariableType::Array(Rc::new(result_type.unwrap_or(VariableType::Any)))
    }
}

impl From<&Vec<Value>> for VariableType {
    fn from(arr: &Vec<Value>) -> Self {
        if arr.len() == 0 {
            return VariableType::Array(Rc::new(VariableType::Any));
        }

        let result_type = arr
            .iter()
            .fold(None, |acc: Option<VariableType>, b| match acc {
                Some(a) => Some(a.merge(&VariableType::from(b))),
                None => Some(VariableType::from(b)),
            });

        VariableType::Array(Rc::new(result_type.unwrap_or(VariableType::Any)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_value_to_value_kind() {
        assert_eq!(VariableType::from(json!(null)), VariableType::Null);
        assert_eq!(VariableType::from(json!(true)), VariableType::Bool);
        assert_eq!(VariableType::from(json!(42)), VariableType::Number);
        assert_eq!(VariableType::from(json!("hello")), VariableType::String);
    }
}

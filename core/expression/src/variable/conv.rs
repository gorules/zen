use crate::variable::Variable;
use crate::vm::helpers::date_time;
use crate::vm::VMError;
use chrono::NaiveDateTime;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{Number, Value};
use std::rc::Rc;

impl From<Value> for Variable {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Variable::Null,
            Value::Bool(b) => Variable::Bool(b),
            Value::Number(n) => {
                Variable::Number(Decimal::from_str_exact(n.as_str()).expect("Allowed number"))
            }
            Value::String(s) => Variable::String(Rc::from(s.as_str())),
            Value::Array(arr) => {
                Variable::from_array(arr.into_iter().map(Variable::from).collect())
            }
            Value::Object(obj) => Variable::from_object(
                obj.into_iter()
                    .map(|(k, v)| (k, Variable::from(v)))
                    .collect(),
            ),
        }
    }
}

impl From<&Value> for Variable {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Variable::Null,
            Value::Bool(b) => Variable::Bool(*b),
            Value::Number(n) => {
                Variable::Number(Decimal::from_str_exact(n.as_str()).expect("Allowed number"))
            }
            Value::String(s) => Variable::String(Rc::from(s.as_str())),
            Value::Array(arr) => Variable::from_array(arr.iter().map(Variable::from).collect()),
            Value::Object(obj) => Variable::from_object(
                obj.iter()
                    .map(|(k, v)| (k.clone(), Variable::from(v)))
                    .collect(),
            ),
        }
    }
}

impl From<Variable> for Value {
    fn from(value: Variable) -> Self {
        match value {
            Variable::Null => Value::Null,
            Variable::Bool(b) => Value::Bool(b),
            Variable::Number(n) => Value::Number(Number::from_string_unchecked(n.to_string())),
            Variable::String(s) => Value::String(s.to_string()),
            Variable::Array(arr) => {
                let vec = Rc::try_unwrap(arr)
                    .map(|a| a.into_inner())
                    .unwrap_or_else(|s| {
                        let borrowed = s.borrow();
                        borrowed.clone()
                    });

                Value::Array(vec.into_iter().map(Value::from).collect())
            }
            Variable::Object(obj) => {
                let hmap = Rc::try_unwrap(obj)
                    .map(|a| a.into_inner())
                    .unwrap_or_else(|s| {
                        let borrowed = s.borrow();
                        borrowed.clone()
                    });

                Value::Object(hmap.into_iter().map(|(k, v)| (k, Value::from(v))).collect())
            }
        }
    }
}

impl TryFrom<&Variable> for NaiveDateTime {
    type Error = VMError;

    fn try_from(value: &Variable) -> Result<Self, Self::Error> {
        match value {
            Variable::String(a) => date_time(a),
            #[allow(deprecated)]
            Variable::Number(a) => NaiveDateTime::from_timestamp_opt(
                a.to_i64().ok_or_else(|| VMError::OpcodeErr {
                    opcode: "DateManipulation".into(),
                    message: "Failed to extract date".into(),
                })?,
                0,
            )
            .ok_or_else(|| VMError::ParseDateTimeErr {
                timestamp: a.to_string(),
            }),
            _ => Err(VMError::OpcodeErr {
                opcode: "DateManipulation".into(),
                message: "Unsupported type".into(),
            }),
        }
    }
}

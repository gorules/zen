use crate::variable::Variable;
use rust_decimal::Decimal;
use serde_json::{Number, Value};
use std::rc::Rc;

impl From<Value> for Variable {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Variable::Null,
            Value::Bool(b) => Variable::Bool(b),
            Value::Number(n) => Variable::Number(
                Decimal::from_str_exact(n.as_str())
                    .or_else(|_| Decimal::from_scientific(n.as_str()))
                    .expect("Allowed number"),
            ),
            Value::String(s) => Variable::String(Rc::from(s.as_str())),
            Value::Array(arr) => {
                Variable::from_array(arr.into_iter().map(Variable::from).collect())
            }
            Value::Object(obj) => Variable::from_object(
                obj.into_iter()
                    .map(|(k, v)| (Rc::from(k.as_str()), Variable::from(v)))
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
            Value::Number(n) => Variable::Number(
                Decimal::from_str_exact(n.as_str())
                    .or_else(|_| Decimal::from_scientific(n.as_str()))
                    .expect("Allowed number"),
            ),
            Value::String(s) => Variable::String(Rc::from(s.as_str())),
            Value::Array(arr) => Variable::from_array(arr.iter().map(Variable::from).collect()),
            Value::Object(obj) => Variable::from_object(
                obj.iter()
                    .map(|(k, v)| (Rc::from(k.as_str()), Variable::from(v)))
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
            Variable::Number(n) => {
                Value::Number(Number::from_string_unchecked(n.normalize().to_string()))
            }
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

                Value::Object(
                    hmap.into_iter()
                        .map(|(k, v)| (k.to_string(), Value::from(v)))
                        .collect(),
                )
            }
            Variable::Dynamic(d) => d.to_value(),
        }
    }
}

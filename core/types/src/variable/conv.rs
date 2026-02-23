use crate::variable::Variable;
use rust_decimal::Decimal;
#[cfg(not(feature = "arbitrary_precision"))]
use rust_decimal::prelude::FromPrimitive;
use serde_json::{Number, Value};
use std::rc::Rc;
#[cfg(not(feature = "arbitrary_precision"))]
use std::str::FromStr;

impl From<Value> for Variable {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Variable::Null,
            Value::Bool(b) => Variable::Bool(b),
            Value::Number(n) => {
                #[cfg(feature = "arbitrary_precision")]
                {
                    Variable::Number(
                        Decimal::from_str_exact(n.as_str())
                            .or_else(|_| Decimal::from_scientific(n.as_str()))
                            .expect("Allowed number"),
                    )
                }

                #[cfg(not(feature = "arbitrary_precision"))]
                {
                    if let Some(n) = n.as_u64() {
                        return Variable::Number(n.into());
                    }

                    if let Some(n) = n.as_i64() {
                        return Variable::Number(n.into());
                    }

                    if let Some(n) = n.as_f64() {
                        return Variable::Number(Decimal::from_f64(n).expect("Allowed number"));
                    }

                    unreachable!(
                        "serde_json::Number is always u64, i64, or f64 without arbitrary_precision"
                    )
                }
            }
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
            Value::Number(n) => {
                #[cfg(feature = "arbitrary_precision")]
                {
                    Variable::Number(
                        Decimal::from_str_exact(n.as_str())
                            .or_else(|_| Decimal::from_scientific(n.as_str()))
                            .expect("Allowed number"),
                    )
                }

                #[cfg(not(feature = "arbitrary_precision"))]
                {
                    if let Some(u) = n.as_u64() {
                        return Variable::Number(u.into());
                    }

                    if let Some(i) = n.as_i64() {
                        return Variable::Number(i.into());
                    }

                    if let Some(f) = n.as_f64() {
                        return Variable::Number(Decimal::from_f64(f).expect("Allowed number"));
                    }

                    unreachable!(
                        "serde_json::Number is always u64, i64, or f64 without arbitrary_precision"
                    );
                }
            }
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
                #[cfg(feature = "arbitrary_precision")]
                {
                    Value::Number(Number::from_string_unchecked(n.normalize().to_string()))
                }
                #[cfg(not(feature = "arbitrary_precision"))]
                {
                    Value::Number(
                        Number::from_str(n.normalize().to_string().as_str())
                            .expect("Allowed number"),
                    )
                }
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

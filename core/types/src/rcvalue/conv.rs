use crate::rcvalue::RcValue;
use crate::variable::{ToVariable, Variable};
use rust_decimal::Decimal;
#[cfg(not(feature = "arbitrary_precision"))]
use rust_decimal::prelude::FromPrimitive;
use serde_json::Value;
use std::rc::Rc;

impl ToVariable for RcValue {
    fn to_variable(&self) -> Variable {
        match self {
            RcValue::Null => Variable::Null,
            RcValue::Bool(b) => Variable::Bool(*b),
            RcValue::Number(n) => Variable::Number(*n),
            RcValue::String(s) => Variable::String(Rc::from(s.as_ref())),
            RcValue::Array(arr) => {
                Variable::from_array(arr.iter().map(|v| v.to_variable()).collect())
            }
            RcValue::Object(obj) => Variable::from_object(
                obj.iter()
                    .map(|(k, v)| (Rc::from(k.as_ref()), v.to_variable()))
                    .collect(),
            ),
        }
    }
}

impl From<&Variable> for RcValue {
    fn from(value: &Variable) -> Self {
        match value {
            Variable::Null => RcValue::Null,
            Variable::Bool(b) => RcValue::Bool(*b),
            Variable::Number(n) => RcValue::Number(*n),
            Variable::String(s) => RcValue::String(s.clone()),
            Variable::Array(arr) => {
                let arr = arr.borrow();
                RcValue::Array(arr.iter().map(RcValue::from).collect())
            }
            Variable::Object(obj) => {
                let obj = obj.borrow();
                RcValue::Object(
                    obj.iter()
                        .map(|(k, v)| (k.clone(), RcValue::from(v)))
                        .collect(),
                )
            }
            Variable::Dynamic(d) => RcValue::from(&d.to_value()),
        }
    }
}

impl From<Variable> for RcValue {
    fn from(value: Variable) -> Self {
        Self::from(&value)
    }
}

impl From<&Value> for RcValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => RcValue::Null,
            Value::Bool(b) => RcValue::Bool(*b),
            Value::Number(n) => {
                #[cfg(feature = "arbitrary_precision")]
                {
                    RcValue::Number(
                        Decimal::from_str_exact(n.as_str())
                            .or_else(|_| Decimal::from_scientific(n.as_str()))
                            .expect("Allowed number"),
                    )
                }

                #[cfg(not(feature = "arbitrary_precision"))]
                {
                    let decimal = match n.as_u64() {
                        Some(n) => Decimal::from_u64(n).expect("Allowed number"),
                        None => match n.as_i64() {
                            Some(n) => Decimal::from(n),
                            None => match n.as_f64() {
                                Some(n) => Decimal::from_f64(n).expect("Allowed number"),
                                None => panic!("Invalid number"),
                            },
                        },
                    };

                    RcValue::Number(decimal)
                }
            }
            Value::String(s) => RcValue::String(Rc::from(s.as_str())),
            Value::Array(arr) => RcValue::Array(arr.iter().map(RcValue::from).collect()),
            Value::Object(obj) => RcValue::Object(
                obj.iter()
                    .map(|(k, v)| (Rc::from(k.as_str()), RcValue::from(v)))
                    .collect(),
            ),
        }
    }
}

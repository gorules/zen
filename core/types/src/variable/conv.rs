use crate::rccell::RcCell;
use crate::variable::Variable;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde_json::{Number, Value};
#[cfg(not(feature = "arbitrary_precision"))]
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VariableConversionError {
    #[error("number out of range: {0}")]
    NumberOutOfRange(String),
}

impl Variable {
    fn decimal_from_number(n: &Number) -> Option<Decimal> {
        #[cfg(feature = "arbitrary_precision")]
        {
            Decimal::from_str_exact(n.as_str())
                .or_else(|_| Decimal::from_scientific(n.as_str()))
                .ok()
                .or_else(|| n.as_f64().and_then(Decimal::from_f64))
        }

        #[cfg(not(feature = "arbitrary_precision"))]
        {
            if let Some(u) = n.as_u64() {
                return Some(u.into());
            }
            if let Some(i) = n.as_i64() {
                return Some(i.into());
            }
            n.as_f64().and_then(Decimal::from_f64)
        }
    }
}

impl Variable {
    pub fn try_from_value(value: Value) -> Result<Self, VariableConversionError> {
        match value {
            Value::Null => Ok(Variable::Null),
            Value::Bool(b) => Ok(Variable::Bool(b)),
            Value::Number(n) => Self::decimal_from_number(&n)
                .map(Variable::Number)
                .ok_or_else(|| VariableConversionError::NumberOutOfRange(n.to_string())),
            Value::String(s) => Ok(Variable::String((s.as_str()).into())),
            Value::Array(arr) => Ok(Variable::from_array(
                arr.into_iter()
                    .map(Variable::try_from_value)
                    .collect::<Result<_, _>>()?,
            )),
            Value::Object(obj) => Ok(Variable::from_object(
                obj.into_iter()
                    .map(|(k, v)| {
                        Ok((
                            crate::symbol::Symbol::from(k.as_str()),
                            Variable::try_from_value(v)?,
                        ))
                    })
                    .collect::<Result<_, VariableConversionError>>()?,
            )),
        }
    }
}

impl From<Value> for Variable {
    fn from(value: Value) -> Self {
        match value {
            Value::Number(n) => Variable::decimal_from_number(&n)
                .map(Variable::Number)
                .unwrap_or(Variable::Null),
            Value::Array(arr) => {
                Variable::from_array(arr.into_iter().map(Variable::from).collect())
            }
            Value::Object(obj) => Variable::from_object(
                obj.into_iter()
                    .map(|(k, v)| (crate::symbol::Symbol::from(k.as_str()), Variable::from(v)))
                    .collect(),
            ),
            other => Variable::from(&other),
        }
    }
}

impl From<&Value> for Variable {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Variable::Null,
            Value::Bool(b) => Variable::Bool(*b),
            Value::Number(n) => Variable::decimal_from_number(n)
                .map(Variable::Number)
                .unwrap_or(Variable::Null),
            Value::String(s) => Variable::String((s.as_str()).into()),
            Value::Array(arr) => Variable::from_array(arr.iter().map(Variable::from).collect()),
            Value::Object(obj) => Variable::from_object(
                obj.iter()
                    .map(|(k, v)| (crate::symbol::Symbol::from(k.as_str()), Variable::from(v)))
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn out_of_range_number_converts_to_null() {
        assert_eq!(Variable::from(json!(1e40)), Variable::Null);
        assert_eq!(Variable::from(json!(-1e40)), Variable::Null);
        assert_eq!(
            Variable::from(json!({ "nested": [1e40] })),
            Variable::from(json!({ "nested": [null] }))
        );
    }

    #[test]
    fn underflowing_number_rounds_to_zero() {
        assert_eq!(
            Variable::from(json!(1e-32)),
            Variable::Number(Decimal::ZERO)
        );
    }

    #[test]
    fn try_from_value_rejects_out_of_range_numbers() {
        assert!(matches!(
            Variable::try_from_value(json!(1e40)),
            Err(VariableConversionError::NumberOutOfRange(_))
        ));
        assert!(matches!(
            Variable::try_from_value(json!({ "a": { "b": [1, 2, 1e40] } })),
            Err(VariableConversionError::NumberOutOfRange(_))
        ));
    }

    #[test]
    fn try_from_value_accepts_regular_payloads() {
        let converted =
            Variable::try_from_value(json!({ "a": [1, 2.5, -3], "b": "x", "c": null, "d": true }))
                .unwrap();
        assert_eq!(
            converted,
            Variable::from(json!({ "a": [1, 2.5, -3], "b": "x", "c": null, "d": true }))
        );
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
                let vec = RcCell::try_unwrap(arr)
                    .map(|cell| cell.into_inner())
                    .unwrap_or_else(|s| {
                        let borrowed = s.borrow();
                        borrowed.clone()
                    });

                Value::Array(vec.into_iter().map(Value::from).collect())
            }
            Variable::Object(obj) => {
                let hmap = RcCell::try_unwrap(obj)
                    .map(|cell| cell.into_inner())
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

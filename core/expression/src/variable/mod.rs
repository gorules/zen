pub use crate::variable::map::BumpMap;
pub use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use chrono::NaiveDateTime;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{Number, Value};
use strum_macros::Display;

mod conv;
mod de;
mod map;
mod ser;

use crate::vm::helpers::date_time;
use crate::vm::VMError;
#[allow(unused_imports)]
pub use conv::ToVariable;

#[derive(Debug, PartialEq, Eq, Display)]
pub enum Variable<'arena> {
    Null,
    Bool(bool),
    Number(Decimal),
    String(&'arena str),
    Array(BumpVec<'arena, Variable<'arena>>),
    Object(BumpMap<'arena, &'arena str, Variable<'arena>>),
}

impl<'arena> Variable<'arena> {
    pub fn empty_object(arena: &'arena Bump) -> Self {
        Variable::Object(BumpMap::new_in(arena))
    }

    pub fn empty_array(arena: &'arena Bump) -> Self {
        Variable::Array(BumpVec::new_in(arena))
    }

    pub fn as_str(&self) -> Option<&'arena str> {
        match self {
            Variable::String(s) => Some(*s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&BumpVec<'arena, Variable<'arena>>> {
        match self {
            Variable::Array(arr) => Some(arr),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BumpMap<'arena, &'arena str, Variable<'arena>>> {
        match self {
            Variable::Object(obj) => Some(obj),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Variable::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &str {
        match self {
            Variable::Null => "null",
            Variable::Bool(_) => "bool",
            Variable::Number(_) => "number",
            Variable::String(_) => "string",
            Variable::Array(_) => "array",
            Variable::Object(_) => "object",
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Variable::Null => Value::Null,
            Variable::Bool(b) => Value::Bool(*b),
            Variable::Number(n) => {
                #[cfg(feature = "arbitrary_precision")]
                {
                    Value::Number(Number::from_string_unchecked(n.normalize().to_string()))
                }

                #[cfg(not(feature = "arbitrary_precision"))]
                {
                    if let Some(n_uint) = n.to_u64() {
                        if Decimal::from(n_uint) == *n {
                            return Value::Number(Number::from(n_uint));
                        }
                    }

                    if let Some(n_int) = n.to_i64() {
                        if Decimal::from(n_int) == *n {
                            return Value::Number(Number::from(n_int));
                        }
                    }

                    if let Some(n_float) = n.to_f64() {
                        return Value::Number(Number::from_f64(n_float).unwrap());
                    }

                    Value::Null
                }
            }
            Variable::String(str) => Value::String(str.to_string()),
            Variable::Array(arr) => Value::Array(arr.iter().map(|i| i.to_value()).collect()),
            Variable::Object(obj) => Value::Object(
                obj.iter()
                    .map(|(k, v)| (k.to_string(), v.to_value()))
                    .collect(),
            ),
        }
    }

    pub fn clone_in<'new>(&self, arena: &'new Bump) -> Variable<'new> {
        match self {
            Variable::Null => Variable::Null,
            Variable::Bool(b) => Variable::Bool(*b),
            Variable::Number(n) => Variable::Number(*n),
            Variable::String(s) => Variable::String(arena.alloc_str(s)),
            Variable::Array(arr) => Variable::Array(BumpVec::from_iter_in(
                arr.iter().map(|v| v.clone_in(arena)),
                arena,
            )),
            Variable::Object(obj) => Variable::Object(BumpMap::from_iter_in(
                obj.iter()
                    .map(|(k, v)| (&*arena.alloc_str(k), v.clone_in(arena))),
                arena,
            )),
        }
    }

    pub fn dot(&self, key: &str) -> Option<&Variable<'arena>> {
        key.split('.').try_fold(self, |var, part| match var {
            Variable::Object(obj) => obj.get(part),
            _ => None,
        })
    }

    pub fn dot_mut(&mut self, key: &str) -> Option<&mut Variable<'arena>> {
        key.split('.').try_fold(self, |var, part| match var {
            Variable::Object(obj) => obj.get_mut(part),
            _ => None,
        })
    }

    pub fn dot_insert(
        &mut self,
        arena: &'arena Bump,
        key: &str,
        variable: Variable<'arena>,
    ) -> Option<&mut Variable<'arena>> {
        let mut parts: BumpVec<&'arena str> =
            BumpVec::from_iter_in(key.split('.').map(|p| &*arena.alloc_str(p)), arena);
        let Some(last_part) = parts.pop() else {
            return None;
        };

        let head = parts.iter().try_fold(self, |var, part| match var {
            Variable::Object(obj) => {
                if obj.contains_key(part) {
                    obj.get_mut(part)
                } else {
                    obj.insert(part, Self::empty_object(arena));
                    obj.get_mut(part)
                }
            }
            _ => None,
        })?;

        let Variable::Object(head_obj) = head else {
            return None;
        };

        head_obj.insert(last_part, variable);
        head_obj.get_mut(last_part)
    }
}

impl TryFrom<&Variable<'_>> for NaiveDateTime {
    type Error = VMError;

    fn try_from(value: &Variable<'_>) -> Result<Self, Self::Error> {
        match value {
            Variable::String(a) => date_time(a),
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

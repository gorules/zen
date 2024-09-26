pub use crate::variable::map::BumpMap;
use ahash::HashMap;
pub use bumpalo::collections::Vec as BumpVec;
use chrono::NaiveDateTime;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::rc::Rc;
use strum_macros::Display;

mod conv;
mod de;
mod map;
mod ser;
mod types;

use crate::vm::helpers::date_time;
use crate::vm::VMError;
pub use types::VariableType;

pub type RcCell<T> = Rc<RefCell<T>>;
#[derive(Debug, PartialEq, Eq, Clone, Display)]
pub enum Variable {
    Null,
    Bool(bool),
    Number(Decimal),
    String(Rc<str>),
    Array(RcCell<Vec<Variable>>),
    Object(RcCell<HashMap<String, Variable>>),
}

impl Variable {
    pub fn from_array(arr: Vec<Variable>) -> Self {
        Self::Array(Rc::new(RefCell::new(arr)))
    }

    pub fn from_object(obj: HashMap<String, Variable>) -> Self {
        Self::Object(Rc::new(RefCell::new(obj)))
    }

    pub fn empty_object() -> Self {
        Variable::Object(Default::default())
    }

    pub fn empty_array() -> Self {
        Variable::Array(Default::default())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Variable::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    pub fn as_rc_str(&self) -> Option<Rc<str>> {
        match self {
            Variable::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<RcCell<Vec<Variable>>> {
        match self {
            Variable::Array(arr) => Some(arr.clone()),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<RcCell<HashMap<String, Variable>>> {
        match self {
            Variable::Object(obj) => Some(obj.clone()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Variable::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
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
        Value::from(self.clone())
    }

    pub fn dot(&self, key: &str) -> Option<Variable> {
        key.split('.')
            .try_fold(self.clone(), |var, part| match var {
                Variable::Object(obj) => {
                    let reference = obj.borrow();
                    reference.get(part).cloned()
                }
                _ => None,
            })
    }

    pub fn dot_insert(&mut self, key: &str, variable: Variable) -> Option<Variable> {
        let mut parts = Vec::from_iter(key.split('.'));
        let Some(last_part) = parts.pop() else {
            return None;
        };

        let head = parts.iter().try_fold(self.clone(), |var, part| match var {
            Variable::Object(obj) => {
                let mut obj_ref = obj.borrow_mut();
                Some(match obj_ref.entry(part.to_string()) {
                    Entry::Occupied(occ) => occ.get().clone(),
                    Entry::Vacant(vac) => vac.insert(Self::empty_object()).clone(),
                })
            }
            _ => None,
        })?;

        let Variable::Object(head_obj) = head else {
            return None;
        };

        let mut head_obj_ref = head_obj.borrow_mut();
        head_obj_ref.insert(last_part.to_string(), variable)
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

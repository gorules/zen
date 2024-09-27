pub use crate::variable::map::BumpMap;
use ahash::HashMap;
pub use bumpalo::collections::Vec as BumpVec;
use rust_decimal::Decimal;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::rc::Rc;

mod conv;
mod de;
mod map;
mod ser;
mod types;

pub use de::VariableDeserializer;
pub use types::VariableType;

pub(crate) type RcCell<T> = Rc<RefCell<T>>;
#[derive(Debug, PartialEq, Eq, Clone)]
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

    pub fn is_array(&self) -> bool {
        match self {
            Variable::Array(_) => true,
            _ => false,
        }
    }

    pub fn as_object(&self) -> Option<RcCell<HashMap<String, Variable>>> {
        match self {
            Variable::Object(obj) => Some(obj.clone()),
            _ => None,
        }
    }

    pub fn is_object(&self) -> bool {
        match self {
            Variable::Object(_) => true,
            _ => false,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Variable::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<Decimal> {
        match self {
            Variable::Number(n) => Some(*n),
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

    pub fn dot_insert(&self, key: &str, variable: Variable) -> Option<Variable> {
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

    pub fn merge(&mut self, patch: &Variable) -> Variable {
        merge_variables(self, patch, true);

        self.clone()
    }
}

fn merge_variables(doc: &mut Variable, patch: &Variable, top_level: bool) {
    if !patch.is_object() && !patch.is_array() && top_level {
        return;
    }

    if doc.is_object() && patch.is_object() {
        let map_ref = doc.as_object().unwrap();
        let mut map = map_ref.borrow_mut();

        let patch_ref = patch.as_object().unwrap();
        let patch = patch_ref.borrow();
        for (key, value) in patch.deref() {
            if value == &Variable::Null {
                map.remove(key.as_str());
            } else {
                let entry = map.entry(key.to_string()).or_insert(Variable::Null);
                merge_variables(entry, value, false)
            }
        }
    } else if doc.is_array() && patch.is_array() {
        let arr_ref = doc.as_array().unwrap();
        let mut arr = arr_ref.borrow_mut();

        let patch_ref = patch.as_array().unwrap();
        let patch = patch_ref.borrow();
        arr.extend(patch.clone());
    } else {
        *doc = patch.clone();
    }
}

impl Display for Variable {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Variable::Null => write!(f, "null"),
            Variable::Bool(b) => match *b {
                true => write!(f, "true"),
                false => write!(f, "false"),
            },
            Variable::Number(n) => write!(f, "{n}"),
            Variable::String(s) => write!(f, "\"{s}\""),
            Variable::Array(arr) => {
                let arr = arr.borrow();
                let s = arr
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<String>>()
                    .join(",");
                write!(f, "[{s}]")
            }
            Variable::Object(obj) => {
                let obj = obj.borrow();
                let s = obj
                    .iter()
                    .map(|(k, v)| format!("\"{k}\":{v}"))
                    .collect::<Vec<String>>()
                    .join(",");

                write!(f, "{{{s}}}")
            }
        }
    }
}

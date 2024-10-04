use ahash::HashMap;
use rust_decimal::prelude::Zero;
use rust_decimal::Decimal;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::fmt::{Debug, Display, Formatter};
use std::ops::Deref;
use std::rc::Rc;

mod conv;
mod de;
mod ser;
mod types;

pub use de::VariableDeserializer;
pub use types::VariableType;

pub(crate) type RcCell<T> = Rc<RefCell<T>>;
#[derive(PartialEq, Eq)]
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
        Value::from(self.shallow_clone())
    }

    pub fn dot(&self, key: &str) -> Option<Variable> {
        key.split('.')
            .try_fold(self.shallow_clone(), |var, part| match var {
                Variable::Object(obj) => {
                    let reference = obj.borrow();
                    reference.get(part).map(|v| v.shallow_clone())
                }
                _ => None,
            })
    }

    fn dot_head(&self, key: &str) -> Option<Variable> {
        let mut parts = Vec::from_iter(key.split('.'));
        parts.pop();

        parts
            .iter()
            .try_fold(self.shallow_clone(), |var, part| match var {
                Variable::Object(obj) => {
                    let mut obj_ref = obj.borrow_mut();
                    Some(match obj_ref.entry(part.to_string()) {
                        Entry::Occupied(occ) => occ.get().shallow_clone(),
                        Entry::Vacant(vac) => vac.insert(Self::empty_object()).shallow_clone(),
                    })
                }
                _ => None,
            })
    }
    pub fn dot_remove(&self, key: &str) -> Option<Variable> {
        let last_part = key.split('.').last()?;
        let head = self.dot_head(key)?;
        let Variable::Object(object_ref) = head else {
            return None;
        };

        let mut object = object_ref.borrow_mut();
        object.remove(last_part)
    }

    pub fn dot_insert(&self, key: &str, variable: Variable) -> Option<Variable> {
        let last_part = key.split('.').last()?;
        let head = self.dot_head(key)?;
        let Variable::Object(object_ref) = head else {
            return None;
        };

        let mut object = object_ref.borrow_mut();
        object.insert(last_part.to_string(), variable)
    }

    pub fn merge(&mut self, patch: &Variable) -> Variable {
        merge_variables(self, patch, true);

        self.shallow_clone()
    }

    pub fn shallow_clone(&self) -> Self {
        match self {
            Variable::Null => Variable::Null,
            Variable::Bool(b) => Variable::Bool(*b),
            Variable::Number(n) => Variable::Number(*n),
            Variable::String(s) => Variable::String(s.clone()),
            Variable::Array(a) => Variable::Array(a.clone()),
            Variable::Object(o) => Variable::Object(o.clone()),
        }
    }

    pub fn deep_clone(&self) -> Self {
        match self {
            Variable::Array(a) => {
                let arr = a.borrow();
                Variable::from_array(arr.iter().map(|v| v.deep_clone()).collect())
            }
            Variable::Object(o) => {
                let obj = o.borrow();
                Variable::from_object(
                    obj.iter()
                        .map(|(k, v)| (k.to_string(), v.deep_clone()))
                        .collect(),
                )
            }
            _ => self.shallow_clone(),
        }
    }

    pub fn depth_clone(&self, depth: usize) -> Self {
        match depth.is_zero() {
            true => self.shallow_clone(),
            false => match self {
                Variable::Array(a) => {
                    let arr = a.borrow();
                    Variable::from_array(arr.iter().map(|v| v.depth_clone(depth - 1)).collect())
                }
                Variable::Object(o) => {
                    let obj = o.borrow();
                    Variable::from_object(
                        obj.iter()
                            .map(|(k, v)| (k.to_string(), v.depth_clone(depth - 1)))
                            .collect(),
                    )
                }
                _ => self.shallow_clone(),
            },
        }
    }
}

impl Clone for Variable {
    fn clone(&self) -> Self {
        self.shallow_clone()
    }
}

fn merge_variables(doc: &mut Variable, patch: &Variable, top_level: bool) {
    if !patch.is_object() && !patch.is_array() && top_level {
        return;
    }

    if doc.is_object() && patch.is_object() {
        let map_ref = doc.as_object().unwrap();
        let patch_ref = patch.as_object().unwrap();
        if Rc::ptr_eq(&map_ref, &patch_ref) {
            return;
        }

        let mut map = map_ref.borrow_mut();
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
        let patch_ref = patch.as_array().unwrap();
        if Rc::ptr_eq(&arr_ref, &patch_ref) {
            return;
        }

        let mut arr = arr_ref.borrow_mut();
        let patch = patch_ref.borrow();
        arr.extend(patch.iter().map(|s| s.shallow_clone()));
    } else {
        *doc = patch.shallow_clone();
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

impl Debug for Variable {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

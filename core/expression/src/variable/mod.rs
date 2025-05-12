use ahash::HashMap;
use rust_decimal::prelude::Zero;
use rust_decimal::Decimal;
use serde_json::Value;
use std::any::Any;
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

pub enum Variable {
    Null,
    Bool(bool),
    Number(Decimal),
    String(Rc<str>),
    Array(RcCell<Vec<Variable>>),
    Object(RcCell<HashMap<Rc<str>, Variable>>),
    Dynamic(Rc<dyn DynamicVariable>),
}

pub trait DynamicVariable: Display {
    fn type_name(&self) -> &'static str;

    fn as_any(&self) -> &dyn Any;

    fn to_value(&self) -> Value;
}

impl Variable {
    pub fn from_array(arr: Vec<Self>) -> Self {
        Self::Array(Rc::new(RefCell::new(arr)))
    }

    pub fn from_object(obj: HashMap<Rc<str>, Self>) -> Self {
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

    pub fn as_object(&self) -> Option<RcCell<HashMap<Rc<str>, Variable>>> {
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
            Variable::Dynamic(d) => d.type_name(),
        }
    }

    pub fn dynamic<T: DynamicVariable + 'static>(&self) -> Option<&T> {
        match self {
            Variable::Dynamic(d) => d.as_any().downcast_ref::<T>(),
            _ => None,
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
                    Some(match obj_ref.entry(Rc::from(*part)) {
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
        object.insert(Rc::from(last_part), variable)
    }

    pub fn merge(&mut self, patch: &Variable) -> Variable {
        let _ = merge_variables(self, patch, true, MergeStrategy::InPlace);

        self.shallow_clone()
    }

    pub fn merge_clone(&mut self, patch: &Variable) -> Variable {
        let mut new_self = self.shallow_clone();

        let _ = merge_variables(&mut new_self, patch, true, MergeStrategy::CloneOnWrite);
        new_self
    }

    pub fn shallow_clone(&self) -> Self {
        match self {
            Variable::Null => Variable::Null,
            Variable::Bool(b) => Variable::Bool(*b),
            Variable::Number(n) => Variable::Number(*n),
            Variable::String(s) => Variable::String(s.clone()),
            Variable::Array(a) => Variable::Array(a.clone()),
            Variable::Object(o) => Variable::Object(o.clone()),
            Variable::Dynamic(d) => Variable::Dynamic(d.clone()),
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
                        .map(|(k, v)| (k.clone(), v.deep_clone()))
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
                            .map(|(k, v)| (k.clone(), v.depth_clone(depth - 1)))
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

#[derive(Copy, Clone)]
enum MergeStrategy {
    InPlace,
    CloneOnWrite,
}

fn merge_variables(
    doc: &mut Variable,
    patch: &Variable,
    top_level: bool,
    strategy: MergeStrategy,
) -> bool {
    if patch.is_array() && top_level {
        *doc = patch.shallow_clone();
        return true;
    }

    if !patch.is_object() && top_level {
        return false;
    }

    if doc.is_object() && patch.is_object() {
        let doc_ref = doc.as_object().unwrap();
        let patch_ref = patch.as_object().unwrap();
        if Rc::ptr_eq(&doc_ref, &patch_ref) {
            return false;
        }

        let patch = patch_ref.borrow();
        match strategy {
            MergeStrategy::InPlace => {
                let mut map = doc_ref.borrow_mut();
                for (key, value) in patch.deref() {
                    if value == &Variable::Null {
                        map.remove(key);
                    } else {
                        let entry = map.entry(key.clone()).or_insert(Variable::Null);
                        merge_variables(entry, value, false, strategy);
                    }
                }

                return true;
            }
            MergeStrategy::CloneOnWrite => {
                let mut changed = false;
                let mut new_map = None;

                for (key, value) in patch.deref() {
                    // Get or create the new map if we haven't yet
                    let map = if let Some(ref mut m) = new_map {
                        m
                    } else {
                        let m = doc_ref.borrow().clone();
                        new_map = Some(m);
                        new_map.as_mut().unwrap()
                    };

                    if value == &Variable::Null {
                        // Remove null values
                        if map.remove(key).is_some() {
                            changed = true;
                        }
                    } else {
                        // Handle nested merging
                        let entry = map.entry(key.clone()).or_insert(Variable::Null);
                        if merge_variables(entry, value, false, strategy) {
                            changed = true;
                        }
                    }
                }

                // Only update doc if changes were made
                if changed {
                    if let Some(new_map) = new_map {
                        *doc = Variable::Object(Rc::new(RefCell::new(new_map)));
                    }
                    return true;
                }

                return false;
            }
        }
    } else {
        let new_value = patch.shallow_clone();
        if *doc != new_value {
            *doc = new_value;
            return true;
        }

        return false;
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
            Variable::Dynamic(d) => write!(f, "{d}"),
        }
    }
}

impl Debug for Variable {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self)
    }
}

impl PartialEq for Variable {
    fn eq(&self, other: &Self) -> bool {
        match (&self, &other) {
            (Variable::Null, Variable::Null) => true,
            (Variable::Bool(b1), Variable::Bool(b2)) => b1 == b2,
            (Variable::Number(n1), Variable::Number(n2)) => n1 == n2,
            (Variable::String(s1), Variable::String(s2)) => s1 == s2,
            (Variable::Array(a1), Variable::Array(a2)) => a1 == a2,
            (Variable::Object(obj1), Variable::Object(obj2)) => obj1 == obj2,
            (Variable::Dynamic(d1), Variable::Dynamic(d2)) => Rc::ptr_eq(d1, d2),
            _ => false,
        }
    }
}

impl Eq for Variable {}

mod conv;
mod util;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum VariableType {
    Any,
    Null,
    Bool,
    String,
    Number,
    Constant(Rc<serde_json::Value>),
    Array(Rc<VariableType>),
    Object(HashMap<String, Rc<VariableType>>),
    Closure(Rc<VariableType>),
}

impl VariableType {
    pub fn array(self) -> Self {
        Self::Array(Rc::new(self))
    }
}

impl Default for VariableType {
    fn default() -> Self {
        VariableType::Null
    }
}

impl Display for VariableType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableType::Any => write!(f, "any"),
            VariableType::Null => write!(f, "null"),
            VariableType::Bool => write!(f, "bool"),
            VariableType::String => write!(f, "string"),
            VariableType::Number => write!(f, "number"),
            VariableType::Constant(c) => write!(f, "{c}"),
            VariableType::Array(v) => write!(f, "{v}[]"),
            VariableType::Object(_) => write!(f, "object"),
            VariableType::Closure(rt) => write!(f, "() => {rt}"),
        }
    }
}

impl Hash for VariableType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self {
            VariableType::Any => 0.hash(state),
            VariableType::Null => 1.hash(state),
            VariableType::Bool => 2.hash(state),
            VariableType::String => 3.hash(state),
            VariableType::Number => 4.hash(state),
            VariableType::Closure(rt) => rt.hash(state),
            VariableType::Constant(c) => c.hash(state),
            VariableType::Array(arr) => arr.hash(state),
            VariableType::Object(obj) => {
                let mut pairs: Vec<_> = obj.iter().collect();
                pairs.sort_by_key(|i| i.0);

                Hash::hash(&pairs, state);
            }
        }
    }
}

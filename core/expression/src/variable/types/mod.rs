mod conv;
mod util;

use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Display;
use std::rc::Rc;

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
pub enum VariableType {
    Any,
    Null,
    Bool,
    String,
    Number,
    Constant(Rc<serde_json::Value>),
    Array(Rc<VariableType>),
    Object(HashMap<String, Rc<VariableType>>),
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
        }
    }
}

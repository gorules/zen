mod conv;
mod util;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Display, Write};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub enum VariableType {
    Any,
    Null,
    Bool,
    String,
    Number,
    Date,
    Interval,
    Array(Rc<VariableType>),
    Object(HashMap<Rc<str>, Rc<VariableType>>),

    Const(Rc<str>),
    Enum(Option<Rc<str>>, Vec<Rc<str>>),
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
            VariableType::Date => write!(f, "date"),
            VariableType::Interval => write!(f, "interval"),
            VariableType::Const(c) => write!(f, "\"{c}\""),
            VariableType::Enum(name, e) => {
                if let Some(name) = name {
                    return name.fmt(f);
                }

                let mut first = true;
                for s in e.iter() {
                    if !first {
                        f.write_str(" | ")?;
                    }

                    f.write_char('"')?;
                    f.write_str(s)?;
                    f.write_char('"')?;
                    first = false;
                }

                Ok(())
            }
            VariableType::Array(v) => write!(f, "{v}[]"),
            VariableType::Object(_) => write!(f, "object"),
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
            VariableType::Date => 5.hash(state),
            VariableType::Interval => 6.hash(state),
            VariableType::Const(c) => {
                7.hash(state);
                c.hash(state)
            }
            VariableType::Enum(name, e) => {
                8.hash(state);
                name.hash(state);
                e.hash(state)
            }
            VariableType::Array(arr) => {
                9.hash(state);
                arr.hash(state)
            }
            VariableType::Object(obj) => {
                10.hash(state);

                let mut pairs: Vec<_> = obj.iter().collect();
                pairs.sort_by_key(|i| i.0);

                Hash::hash(&pairs, state);
            }
        }
    }
}

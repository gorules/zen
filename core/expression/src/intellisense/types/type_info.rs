use crate::variable::VariableType;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeInfo {
    pub(crate) kind: Rc<VariableType>,
    pub(crate) error: Option<String>,
}

impl Deref for TypeInfo {
    type Target = VariableType;

    fn deref(&self) -> &Self::Target {
        &self.kind
    }
}

impl Display for TypeInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}

impl Default for TypeInfo {
    fn default() -> Self {
        Self {
            kind: Rc::new(VariableType::Any),
            error: None,
        }
    }
}

impl From<VariableType> for TypeInfo {
    fn from(value: VariableType) -> Self {
        Self {
            kind: Rc::new(value),
            error: None,
        }
    }
}

impl From<Rc<VariableType>> for TypeInfo {
    fn from(value: Rc<VariableType>) -> Self {
        Self {
            kind: value,
            error: None,
        }
    }
}

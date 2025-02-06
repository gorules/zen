use crate::Variable;
use ahash::HashMap;
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredVariable {
    Null,
    Bool(bool),
    Number(Decimal),
    String(Arc<str>),
    Array(Arc<Vec<StoredVariable>>),
    Object(Arc<HashMap<String, StoredVariable>>),
}

impl From<StoredVariable> for Variable {
    fn from(value: StoredVariable) -> Self {
        match value {
            StoredVariable::Null => Variable::Null,
            StoredVariable::Bool(b) => Variable::Bool(b),
            StoredVariable::Number(n) => Variable::Number(n),
            StoredVariable::String(s) => Variable::String(Rc::from(s.as_ref())),
            StoredVariable::Array(arr) => Variable::Array(Rc::new(RefCell::new(
                arr.iter().cloned().map(Variable::from).collect(),
            ))),
            StoredVariable::Object(obj) => Variable::Object(Rc::new(RefCell::new(
                obj.iter()
                    .map(|(k, v)| (k.clone(), Variable::from(v.clone())))
                    .collect(),
            ))),
        }
    }
}

impl From<Variable> for StoredVariable {
    fn from(value: Variable) -> Self {
        match value {
            Variable::Null => StoredVariable::Null,
            Variable::Bool(b) => StoredVariable::Bool(b),
            Variable::Number(n) => StoredVariable::Number(n),
            Variable::String(s) => StoredVariable::String(Arc::from(s.as_ref())),
            Variable::Array(arr) => {
                let a = arr.borrow();
                StoredVariable::Array(Arc::new(
                    a.iter().cloned().map(StoredVariable::from).collect(),
                ))
            }
            Variable::Object(obj) => {
                let o = obj.borrow();
                StoredVariable::Object(Arc::new(
                    o.iter()
                        .map(|(k, v)| (k.clone(), StoredVariable::from(v.clone())))
                        .collect(),
                ))
            }
        }
    }
}

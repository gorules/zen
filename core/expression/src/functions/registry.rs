use crate::functions::arguments::Arguments;
use crate::functions::{DeprecatedFunction, FunctionKind, InternalFunction};
use crate::variable::VariableType;
use crate::Variable;
use nohash_hasher::{BuildNoHashHasher, IsEnabled};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use strum::IntoEnumIterator;

impl IsEnabled for InternalFunction {}
impl IsEnabled for DeprecatedFunction {}

pub struct FunctionRegistry {
    internal_functions:
        HashMap<InternalFunction, Rc<dyn FunctionDefinition>, BuildNoHashHasher<InternalFunction>>,
    deprecated_functions: HashMap<
        DeprecatedFunction,
        Rc<dyn FunctionDefinition>,
        BuildNoHashHasher<DeprecatedFunction>,
    >,
}

impl FunctionRegistry {
    thread_local!(
        static INSTANCE: RefCell<FunctionRegistry> = RefCell::new(FunctionRegistry::new_internal())
    );

    pub fn get_definition(kind: &FunctionKind) -> Option<Rc<dyn FunctionDefinition>> {
        match kind {
            FunctionKind::Internal(internal) => {
                Self::INSTANCE.with_borrow(|i| i.internal_functions.get(&internal).cloned())
            }
            FunctionKind::Deprecated(deprecated) => {
                Self::INSTANCE.with_borrow(|i| i.deprecated_functions.get(&deprecated).cloned())
            }
            FunctionKind::Closure(_) => None,
        }
    }

    fn new_internal() -> Self {
        let internal_functions = InternalFunction::iter()
            .map(|i| (i.clone(), (&i).into()))
            .collect();

        let deprecated_functions = DeprecatedFunction::iter()
            .map(|i| (i.clone(), (&i).into()))
            .collect();

        Self {
            internal_functions,
            deprecated_functions,
        }
    }
}

pub trait FunctionDefinition {
    fn required_parameters(&self) -> usize;
    fn optional_parameters(&self) -> usize;
    fn check_types(&self, args: &[Rc<VariableType>]) -> FunctionTypecheck;
    fn call(&self, args: Arguments) -> anyhow::Result<Variable>;
    fn param_type(&self, index: usize) -> String;
    fn return_type(&self) -> String;
}

#[derive(Debug, Default)]
pub struct FunctionTypecheck {
    pub general: Option<String>,
    pub arguments: Vec<(usize, String)>,
    pub return_type: VariableType,
}

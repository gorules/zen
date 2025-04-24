use crate::functions::arguments::Arguments;
use crate::functions::FunctionKind;
use crate::variable::VariableType;
use crate::Variable;
use std::cell::RefCell;
use std::rc::Rc;

pub struct FunctionRegistry {
    // functions: AHashMap<String, Rc<dyn FunctionDefinition>>,
}

impl FunctionRegistry {
    thread_local!(
        static INSTANCE: RefCell<FunctionRegistry> = RefCell::new(FunctionRegistry::new_internal())
    );

    pub fn get_definition(kind: &FunctionKind) -> Option<Rc<dyn FunctionDefinition>> {
        // Self::INSTANCE.with_borrow(|s| s.functions.get(key).cloned())

        match kind {
            FunctionKind::Internal(internal) => Some(internal.into()),
            FunctionKind::Deprecated(deprecated) => Some(deprecated.into()),
            FunctionKind::Closure(_) => None,
        }
    }

    fn new_internal() -> Self {
        Self {}
    }
}

pub trait FunctionDefinition {
    fn required_parameters(&self) -> usize;
    fn optional_parameters(&self) -> usize;
    fn check_types(&self, args: &[VariableType]) -> Result<VariableType, String>;
    fn call(&self, args: Arguments) -> anyhow::Result<Variable>;
}

// #[derive(Clone)]
// pub struct DynamicFunction {
//     pub name: String,
//     pub type_checker: Rc<dyn Fn(&[VariableType]) -> Result<VariableType, String>>,
//     pub parameter_count: usize,
// }

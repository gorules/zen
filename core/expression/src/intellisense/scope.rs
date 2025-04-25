use crate::variable::VariableType;

#[derive(Clone, Debug)]
pub struct IntelliSenseScope<'a> {
    pub root_data: &'a VariableType,
    pub current_data: &'a VariableType,
    pub pointer_data: &'a VariableType,
}

use crate::variable::VariableType;

#[derive(Clone, Debug)]
pub struct IntelliSenseScope {
    pub root_data: VariableType,
    pub current_data: VariableType,
    pub pointer_data: VariableType,
}

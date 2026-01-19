use crate::variable::VariableType;
use ahash::HashMap;
use std::rc::Rc;

#[derive(Clone, Debug, Default)]
pub struct IntelliSenseScope {
    pub root_data: VariableType,
    pub current_data: VariableType,
    pub pointer_data: VariableType,
    pub aliases: HashMap<Rc<str>, VariableType>,
}

impl IntelliSenseScope {
    pub fn get_alias(&self, name: &str) -> Option<&VariableType> {
        self.aliases.get(name)
    }
}

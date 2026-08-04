use crate::variable::Variable;
use smallvec::SmallVec;
use zen_types::symbol::Symbol;

#[derive(Debug, Clone)]
pub struct Scope {
    base: Variable,
    locals: SmallVec<[(Symbol, Variable); 4]>,
}

impl Default for Scope {
    fn default() -> Self {
        Self::new(Variable::Null)
    }
}

impl Scope {
    pub fn new(base: Variable) -> Self {
        Self {
            base,
            locals: SmallVec::new(),
        }
    }

    pub fn base(&self) -> &Variable {
        &self.base
    }

    pub fn base_mut(&mut self) -> &mut Variable {
        &mut self.base
    }

    pub fn set_base(&mut self, base: Variable) {
        self.base = base;
        self.locals.clear();
    }

    pub fn locals(&self) -> &[(Symbol, Variable)] {
        &self.locals
    }

    pub fn clear_locals(&mut self) {
        self.locals.clear();
    }

    pub fn set_local(&mut self, name: Symbol, value: Variable) {
        match self
            .locals
            .iter_mut()
            .find(|(key, _)| key.as_str() == name.as_str())
        {
            Some(slot) => slot.1 = value,
            None => self.locals.push((name, value)),
        }
    }

    pub fn local(&self, name: &Symbol) -> Option<&Variable> {
        self.locals
            .iter()
            .find(|(key, _)| key.as_str() == name.as_str())
            .map(|(_, value)| value)
    }

    pub fn local_str(&self, name: &str) -> Option<&Variable> {
        self.locals
            .iter()
            .find(|(key, _)| key.as_str() == name)
            .map(|(_, value)| value)
    }

    pub fn get_str(&self, name: &str) -> Option<Variable> {
        if let Some(value) = self.local_str(name) {
            return Some(value.clone());
        }

        match &self.base {
            Variable::Object(object) => object.borrow().get_str(name).cloned(),
            _ => None,
        }
    }

    pub fn get(&self, name: &Symbol) -> Option<Variable> {
        if let Some(value) = self.local(name) {
            return Some(value.clone());
        }

        match &self.base {
            Variable::Object(object) => object.borrow().get(name).cloned(),
            _ => None,
        }
    }

    pub fn materialize(&self) -> Variable {
        if self.locals.is_empty() {
            return self.base.shallow_clone();
        }

        let Variable::Object(base) = &self.base else {
            return self.base.shallow_clone();
        };

        let mut map = base.borrow().clone();
        for (key, value) in &self.locals {
            map.insert(key.clone(), value.clone());
        }

        Variable::from_object(map)
    }
}

impl From<Variable> for Scope {
    fn from(value: Variable) -> Self {
        Scope::new(value)
    }
}

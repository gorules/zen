use ahash::{HashSet, HashSetExt};
use std::ops::Deref;
use std::rc::Rc;
use zen_types::variable::Variable;

pub(crate) const ZEN_RESERVED_PROPERTIES: &[&str] = &["$nodes", "$params"];

pub(crate) struct VariableCleaner {
    visited: HashSet<usize>,
}

impl VariableCleaner {
    pub fn new() -> Self {
        Self {
            visited: HashSet::new(),
        }
    }

    pub fn clean(&mut self, var: &Variable) {
        match var {
            Variable::Null
            | Variable::Bool(_)
            | Variable::Number(_)
            | Variable::String(_)
            | Variable::Dynamic(_) => {}

            Variable::Array(arr) => {
                let ptr = Rc::as_ptr(arr) as usize;
                if !self.visited.insert(ptr) {
                    return;
                }

                let items = arr.borrow();
                for item in items.iter() {
                    self.clean(item);
                }
            }

            Variable::Object(obj) => {
                let ptr = Rc::as_ptr(obj) as usize;
                if !self.visited.insert(ptr) {
                    return;
                }

                let mut map = obj.borrow_mut();
                for key in ZEN_RESERVED_PROPERTIES {
                    map.remove(*key);
                }

                for (_, value) in map.iter() {
                    self.clean(value);
                }
            }
        }
    }

    pub fn clone_clean(&mut self, var: &Variable) -> Variable {
        match var {
            Variable::Null
            | Variable::Bool(_)
            | Variable::Number(_)
            | Variable::String(_)
            | Variable::Dynamic(_) => var.shallow_clone(),

            Variable::Array(arr) => {
                let ptr = Rc::as_ptr(&arr) as usize;
                if !self.visited.insert(ptr) {
                    return Variable::Array(arr.clone());
                }

                let items = arr.borrow();
                Variable::from_array(items.iter().map(|v| self.clone_clean(v)).collect())
            }

            Variable::Object(obj) => {
                let ptr = Rc::as_ptr(obj) as usize;
                if !self.visited.insert(ptr) {
                    return Variable::Object(obj.clone());
                }

                let map = obj.borrow();
                let will_remove_key = map
                    .keys()
                    .any(|k| ZEN_RESERVED_PROPERTIES.contains(&k.as_ref()));
                if !will_remove_key {
                    return Variable::Object(obj.clone());
                }

                let mut new_map = map.deref().clone();
                for key in ZEN_RESERVED_PROPERTIES {
                    new_map.remove(*key);
                }

                let cleaned_map = new_map
                    .into_iter()
                    .map(|(k, v)| (k, self.clone_clean(&v)))
                    .collect();

                Variable::from_object(cleaned_map)
            }
        }
    }
}

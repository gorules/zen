use ahash::{HashSet, HashSetExt};
use std::rc::Rc;
use zen_types::variable::Variable;

const RESERVED_KEYS: &[&str] = &["$nodes"];

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
                for key in RESERVED_KEYS {
                    map.remove(*key);
                }

                for (_, value) in map.iter() {
                    self.clean(value);
                }
            }
        }
    }
}

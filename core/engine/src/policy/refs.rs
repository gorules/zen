use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt};
use zen_expression::variable::Variable;

pub struct RefPoolIndex {
    by_target: HashMap<Arc<str>, HashMap<Rc<str>, Variable>>,
}

impl RefPoolIndex {
    pub fn from_input(input: &Variable, ref_targets: impl IntoIterator<Item = Arc<str>>) -> Self {
        let mut by_target: HashMap<Arc<str>, HashMap<Rc<str>, Variable>> = HashMap::new();
        for target in ref_targets {
            let pool: HashMap<Rc<str>, Variable> = input
                .dot(target.as_ref())
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.borrow()
                        .iter()
                        .filter_map(|item| {
                            let id = item.dot("id")?.as_rc_str()?;
                            Some((id, item.shallow_clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            by_target.insert(target, pool);
        }
        Self { by_target }
    }

    pub fn contains(&self, target: &str, id: &Rc<str>) -> bool {
        self.by_target
            .get(target)
            .is_some_and(|p| p.contains_key(id))
    }

    pub fn pool_for(&self, target: &str) -> Option<&HashMap<Rc<str>, Variable>> {
        self.by_target.get(target)
    }
}

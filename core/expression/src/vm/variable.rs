use crate::variable::Variable;
use ahash::{HashMap, HashMapExt};
use std::rc::Rc;

pub(crate) struct IntervalObject {
    pub(crate) left_bracket: Rc<str>,
    pub(crate) right_bracket: Rc<str>,
    pub(crate) left: Variable,
    pub(crate) right: Variable,
}

impl IntervalObject {
    pub fn to_variable(&self) -> Variable {
        let mut tree = HashMap::new();

        tree.insert(
            "_symbol".to_string(),
            Variable::String("Interval".to_string().into()),
        );
        tree.insert(
            "left_bracket".to_string(),
            Variable::String(self.left_bracket.clone()),
        );
        tree.insert(
            "right_bracket".to_string(),
            Variable::String(self.right_bracket.clone()),
        );
        tree.insert("left".to_string(), self.left.clone());
        tree.insert("right".to_string(), self.right.clone());

        Variable::from_object(tree)
    }

    pub(crate) fn try_from_object(var: Variable) -> Option<IntervalObject> {
        let Variable::Object(tree) = var else {
            return None;
        };

        let tree_ref = tree.borrow();
        if tree_ref.get("_symbol")?.as_str()? != "Interval" {
            return None;
        }

        let left_bracket = tree_ref.get("left_bracket")?.as_rc_str()?;
        let right_bracket = tree_ref.get("right_bracket")?.as_rc_str()?;
        let left = tree_ref.get("left")?.clone();
        let right = tree_ref.get("right")?.clone();

        Some(Self {
            left_bracket,
            right_bracket,
            right,
            left,
        })
    }
}

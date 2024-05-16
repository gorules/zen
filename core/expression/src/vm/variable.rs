use bumpalo::Bump;

pub use crate::variable::Variable;
use crate::variable::{BumpMap, ToVariable};

pub(crate) struct IntervalObject<'a> {
    pub(crate) left_bracket: &'a str,
    pub(crate) right_bracket: &'a str,
    pub(crate) left: &'a Variable<'a>,
    pub(crate) right: &'a Variable<'a>,
}

impl<'arena> ToVariable<'arena> for IntervalObject<'arena> {
    type Error = ();

    fn to_variable(&self, arena: &'arena Bump) -> Result<Variable<'arena>, Self::Error> {
        let mut tree = BumpMap::new_in(arena);

        tree.insert("_symbol", Variable::String("Interval"));
        tree.insert("left_bracket", Variable::String(self.left_bracket));
        tree.insert("right_bracket", Variable::String(self.right_bracket));
        tree.insert("left", self.left.clone());
        tree.insert("right", self.right.clone());

        Ok(Variable::Object(tree))
    }
}

impl<'a> IntervalObject<'a> {
    pub(crate) fn try_from_object(var: &'a Variable<'a>) -> Option<IntervalObject> {
        let Variable::Object(tree) = var else {
            return None;
        };

        if tree.get("_symbol")?.as_str()? != "Interval" {
            return None;
        }

        let left_bracket = tree.get("left_bracket")?.as_str()?;
        let right_bracket = tree.get("right_bracket")?.as_str()?;
        let left = tree.get("left")?;
        let right = tree.get("right")?;

        Some(Self {
            left_bracket,
            right_bracket,
            right,
            left,
        })
    }
}

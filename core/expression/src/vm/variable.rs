use crate::lexer::Bracket;
use crate::variable::Variable;
use ahash::{HashMap, HashMapExt};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

pub(crate) struct IntervalObject {
    pub(crate) left_bracket: Bracket,
    pub(crate) right_bracket: Bracket,
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
            Variable::Number(Decimal::from(self.left_bracket as usize)),
        );
        tree.insert(
            "right_bracket".to_string(),
            Variable::Number(Decimal::from(self.right_bracket as usize)),
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

        let left_bracket = tree_ref.get("left_bracket")?.as_number()?.to_usize()?;
        let right_bracket = tree_ref.get("right_bracket")?.as_number()?.to_usize()?;
        let left = tree_ref.get("left")?.clone();
        let right = tree_ref.get("right")?.clone();

        Some(Self {
            left_bracket: Bracket::from_repr(left_bracket)?,
            right_bracket: Bracket::from_repr(right_bracket)?,
            right,
            left,
        })
    }

    pub(crate) fn to_array(&self) -> Option<Vec<Variable>> {
        let start = match &self.left_bracket {
            Bracket::LeftParenthesis => self.left.as_number()?.to_usize()? + 1,
            Bracket::LeftSquareBracket => self.left.as_number()?.to_usize()?,
            _ => return None,
        };

        let end = match &self.right_bracket {
            Bracket::RightParenthesis => self.right.as_number()?.to_usize()? - 1,
            Bracket::RightSquareBracket => self.right.as_number()?.to_usize()?,
            _ => return None,
        };

        let list = (start..=end)
            .map(|n| Variable::Number(Decimal::from(n)))
            .collect::<Vec<_>>();

        Some(list)
    }
}

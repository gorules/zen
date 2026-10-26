use crate::lane::date::Date;
use crate::lexer::Bracket;
use crate::variable::DynamicVariable;
use crate::Variable;
use anyhow::anyhow;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use std::any::Any;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone)]
pub(crate) struct Interval {
    pub left_bracket: Bracket,
    pub right_bracket: Bracket,
    pub left: IntervalData,
    pub right: IntervalData,
}

impl DynamicVariable for Interval {
    fn type_name(&self) -> &'static str {
        "interval"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn to_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl Interval {
    pub fn to_array(&self) -> Option<Vec<Variable>> {
        let (left, right) = match (&self.left, &self.right) {
            (IntervalData::Number(l), IntervalData::Number(r)) => (*l, *r),
            _ => return None,
        };

        let start = match &self.left_bracket {
            Bracket::LeftParenthesis => left.to_i64()? + 1,
            Bracket::LeftSquareBracket => left.to_i64()?,
            _ => return None,
        };

        let end = match &self.right_bracket {
            Bracket::RightParenthesis => right.to_i64()? - 1,
            Bracket::RightSquareBracket => right.to_i64()?,
            _ => return None,
        };

        let list = (start..=end)
            .map(|n| Variable::Number(Decimal::from(n)))
            .collect::<Vec<_>>();

        Some(list)
    }

    pub fn includes(&self, v: IntervalData) -> anyhow::Result<bool> {
        let mut is_open = false;
        let l = &self.left;
        let r = &self.right;

        let first = match &self.left_bracket {
            Bracket::LeftParenthesis => l < &v,
            Bracket::LeftSquareBracket => l <= &v,
            Bracket::RightParenthesis => {
                is_open = true;
                l > &v
            }
            Bracket::RightSquareBracket => {
                is_open = true;
                l >= &v
            }
            _ => return Err(anyhow!("Unsupported bracket")),
        };

        let second = match &self.right_bracket {
            Bracket::RightParenthesis => r > &v,
            Bracket::RightSquareBracket => r >= &v,
            Bracket::LeftParenthesis => r < &v,
            Bracket::LeftSquareBracket => r <= &v,
            _ => return Err(anyhow!("Unsupported bracket")),
        };

        let open_stmt = is_open && (first || second);
        let closed_stmt = !is_open && first && second;

        Ok(open_stmt || closed_stmt)
    }
}

impl Display for Interval {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}..{}{}",
            self.left_bracket, self.left, self.right, self.right_bracket
        )
    }
}

#[derive(Debug, Clone, PartialOrd, PartialEq, Ord, Eq)]
pub(crate) enum IntervalData {
    Number(Decimal),
    Date(Date),
}

impl Display for IntervalData {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            IntervalData::Number(n) => write!(f, "{n}"),
            IntervalData::Date(d) => write!(f, "{d}"),
        }
    }
}

use std::hash::{Hash, Hasher};

use nohash_hasher::IsEnabled;
use strum_macros::{Display, EnumString};

/// Contains information from lexical analysis
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Token<'a> {
    pub span: (usize, usize),
    pub kind: TokenKind,
    pub value: &'a str,
}

/// Classification of tokens
#[derive(Debug, PartialEq, Eq, Clone, Copy, Display)]
pub enum TokenKind {
    Identifier(Identifier),
    Boolean(bool),
    Number,
    String,
    Operator(Operator),
    Bracket(Bracket),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display)]
pub enum Identifier {
    ContextReference,  // $
    RootReference,     // $root
    CallbackReference, // #
    Null,              // null
    Variable,
}

impl From<&str> for Identifier {
    fn from(value: &str) -> Self {
        match value {
            "$" => Identifier::ContextReference,
            "$root" => Identifier::RootReference,
            "#" => Identifier::CallbackReference,
            "null" => Identifier::Null,
            _ => Identifier::Variable,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display)]
pub enum Operator {
    Arithmetic(ArithmeticOperator),
    Logical(LogicalOperator),
    Comparison(ComparisonOperator),
    Range,        // ..
    Comma,        // ,
    Slice,        // :
    Dot,          // .
    QuestionMark, // ?
}

impl TryFrom<&str> for Operator {
    type Error = strum::ParseError;

    fn try_from(operator: &str) -> Result<Self, Self::Error> {
        match operator {
            ".." => Ok(Operator::Range),
            "," => Ok(Operator::Comma),
            ":" => Ok(Operator::Slice),
            "." => Ok(Operator::Dot),
            "?" => Ok(Operator::QuestionMark),
            _ => ArithmeticOperator::try_from(operator)
                .map(Operator::Arithmetic)
                .or_else(|_| LogicalOperator::try_from(operator).map(Operator::Logical))
                .or_else(|_| ComparisonOperator::try_from(operator).map(Operator::Comparison)),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display, EnumString)]
pub enum ArithmeticOperator {
    #[strum(serialize = "+")]
    Add,
    #[strum(serialize = "-")]
    Subtract,
    #[strum(serialize = "*")]
    Multiply,
    #[strum(serialize = "/")]
    Divide,
    #[strum(serialize = "%")]
    Modulus,
    #[strum(serialize = "^")]
    Power,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display, EnumString)]
pub enum LogicalOperator {
    #[strum(serialize = "and")]
    And,
    #[strum(serialize = "or")]
    Or,
    #[strum(serialize = "not", serialize = "!")]
    Not,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display, EnumString)]
pub enum ComparisonOperator {
    #[strum(serialize = "==")]
    Equal,
    #[strum(serialize = "!=")]
    NotEqual,
    #[strum(serialize = "<")]
    LessThan,
    #[strum(serialize = ">")]
    GreaterThan,
    #[strum(serialize = "<=")]
    LessThanOrEqual,
    #[strum(serialize = ">=")]
    GreaterThanOrEqual,
    #[strum(serialize = "in")]
    In,
    #[strum(serialize = "not in")]
    NotIn,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display, EnumString)]
pub enum Bracket {
    #[strum(serialize = "(")]
    LeftParenthesis,
    #[strum(serialize = ")")]
    RightParenthesis,
    #[strum(serialize = "[")]
    LeftSquareBracket,
    #[strum(serialize = "]")]
    RightSquareBracket,
}

impl Operator {
    pub fn variant(&self) -> u8 {
        match &self {
            Operator::Arithmetic(a) => match a {
                ArithmeticOperator::Add => 1,
                ArithmeticOperator::Subtract => 2,
                ArithmeticOperator::Multiply => 3,
                ArithmeticOperator::Divide => 4,
                ArithmeticOperator::Modulus => 5,
                ArithmeticOperator::Power => 6,
            },
            Operator::Logical(l) => match l {
                LogicalOperator::And => 7,
                LogicalOperator::Or => 8,
                LogicalOperator::Not => 9,
            },
            Operator::Comparison(c) => match c {
                ComparisonOperator::Equal => 10,
                ComparisonOperator::NotEqual => 11,
                ComparisonOperator::LessThan => 12,
                ComparisonOperator::GreaterThan => 13,
                ComparisonOperator::LessThanOrEqual => 14,
                ComparisonOperator::GreaterThanOrEqual => 15,
                ComparisonOperator::In => 16,
                ComparisonOperator::NotIn => 17,
            },
            Operator::Range => 18,
            Operator::Comma => 19,
            Operator::Slice => 20,
            Operator::Dot => 21,
            Operator::QuestionMark => 22,
        }
    }
}

impl Hash for Operator {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u8(self.variant());
    }
}

impl IsEnabled for Operator {}

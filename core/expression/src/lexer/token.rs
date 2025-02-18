use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use nohash_hasher::IsEnabled;
use strum_macros::{Display, EnumIter, EnumString, FromRepr, IntoStaticStr};

/// Contains information from lexical analysis
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Token<'a> {
    pub span: (u32, u32),
    pub kind: TokenKind,
    pub value: &'a str,
}

/// Classification of tokens
#[derive(Debug, PartialEq, Eq, Clone, Copy, Display)]
pub enum TokenKind {
    Identifier(Identifier),
    Boolean(bool),
    Number,
    QuotationMark(QuotationMark),
    Literal,
    Operator(Operator),
    Bracket(Bracket),
    TemplateString(TemplateString),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display, EnumString, IntoStaticStr)]
pub enum Identifier {
    #[strum(serialize = "$")]
    ContextReference,
    #[strum(serialize = "$root")]
    RootReference,
    #[strum(serialize = "#")]
    CallbackReference,
    #[strum(serialize = "null")]
    Null,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display, EnumString, IntoStaticStr)]
pub enum QuotationMark {
    #[strum(serialize = "'")]
    SingleQuote,
    #[strum(serialize = "\"")]
    DoubleQuote,
    #[strum(serialize = "`")]
    Backtick,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, EnumString, IntoStaticStr)]
pub enum TemplateString {
    #[strum(serialize = "${")]
    ExpressionStart,
    #[strum(serialize = "}")]
    ExpressionEnd,
}

impl Display for TemplateString {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match *self {
            TemplateString::ExpressionStart => ::core::fmt::Display::fmt("${", f),
            TemplateString::ExpressionEnd => ::core::fmt::Display::fmt("}}", f),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
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

impl Display for Operator {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Operator::Arithmetic(a) => write!(f, "{a}"),
            Operator::Logical(l) => write!(f, "{l}"),
            Operator::Comparison(c) => write!(f, "{c}"),
            Operator::Range => write!(f, ".."),
            Operator::Comma => write!(f, ","),
            Operator::Slice => write!(f, ":"),
            Operator::Dot => write!(f, "."),
            Operator::QuestionMark => write!(f, "?"),
        }
    }
}

impl FromStr for Operator {
    type Err = strum::ParseError;

    fn from_str(operator: &str) -> Result<Self, Self::Err> {
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
    #[strum(serialize = "??")]
    NullishCoalescing,
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

#[derive(Debug, PartialEq, Eq, Clone, Copy, EnumString, IntoStaticStr, EnumIter, FromRepr)]
pub enum Bracket {
    #[strum(serialize = "(")]
    LeftParenthesis,
    #[strum(serialize = ")")]
    RightParenthesis,
    #[strum(serialize = "[")]
    LeftSquareBracket,
    #[strum(serialize = "]")]
    RightSquareBracket,
    #[strum(serialize = "{")]
    LeftCurlyBracket,
    #[strum(serialize = "}")]
    RightCurlyBracket,
}

impl Display for Bracket {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match *self {
            Bracket::LeftParenthesis => ::core::fmt::Display::fmt("(", f),
            Bracket::RightParenthesis => ::core::fmt::Display::fmt(")", f),
            Bracket::LeftSquareBracket => ::core::fmt::Display::fmt("[", f),
            Bracket::RightSquareBracket => ::core::fmt::Display::fmt("]", f),
            Bracket::LeftCurlyBracket => ::core::fmt::Display::fmt("{", f),
            Bracket::RightCurlyBracket => ::core::fmt::Display::fmt("}}", f),
        }
    }
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
                LogicalOperator::NullishCoalescing => 10,
            },
            Operator::Comparison(c) => match c {
                ComparisonOperator::Equal => 11,
                ComparisonOperator::NotEqual => 12,
                ComparisonOperator::LessThan => 13,
                ComparisonOperator::GreaterThan => 14,
                ComparisonOperator::LessThanOrEqual => 15,
                ComparisonOperator::GreaterThanOrEqual => 16,
                ComparisonOperator::In => 17,
                ComparisonOperator::NotIn => 18,
            },
            Operator::Range => 19,
            Operator::Comma => 20,
            Operator::Slice => 21,
            Operator::Dot => 22,
            Operator::QuestionMark => 23,
        }
    }
}

impl Hash for Operator {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u8(self.variant());
    }
}

impl IsEnabled for Operator {}

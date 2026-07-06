use serde::Serialize;

use crate::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NlToken {
    pub token: NlTokenKind,
    pub span: (u32, u32),
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<EditHint>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum NlTokenKind {
    GroupOpen,
    GroupClose,
    ListOpen,
    ListClose,
    Comma,
    EnumList {
        selected: Vec<Box<str>>,
    },

    Context,
    Root,
    Null,

    Field {
        path: Vec<Box<str>>,
        ty: TypeTag,
    },
    Element {
        #[serde(skip_serializing_if = "Option::is_none")]
        alias: Option<Box<str>>,
    },

    Number {
        value: Box<str>,
    },
    Str {
        value: Box<str>,
    },
    Bool {
        value: bool,
    },

    Op {
        sym: OpSym,
        implied: bool,
        between: bool,
    },
    Word {
        sym: WordSym,
    },
    Assign,
    StmtEnd,
    Func {
        sym: Box<str>,
        closure: bool,
    },
    Method {
        sym: Box<str>,
    },

    TemplateOpen,
    TemplateText {
        value: Box<str>,
    },
    TemplateClose,

    IntervalOpen {
        inclusive: bool,
    },
    IntervalClose {
        inclusive: bool,
    },

    Code {
        source: Box<str>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum TypeTag {
    Number,
    String,
    Bool,
    Date,
    Interval,
    Object,
    Null,
    Unknown,
    Enum { index: u32 },
    Array { items: Box<TypeTag> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EditHint {
    DatePicker,
    Select { options: u32 },
    MultiSelect { options: u32 },
    OpSelect { options: Vec<OpChoice> },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpChoice {
    pub sym: OpSym,
    pub source: &'static str,
}

impl From<OpSym> for OpChoice {
    fn from(sym: OpSym) -> Self {
        Self {
            sym,
            source: sym.source(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum OpSym {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
    Ne,
    In,
    NotIn,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    And,
    Or,
    Not,
    Coalesce,
}

impl OpSym {
    pub fn source(&self) -> &'static str {
        match self {
            OpSym::Gt => ">",
            OpSym::Gte => ">=",
            OpSym::Lt => "<",
            OpSym::Lte => "<=",
            OpSym::Eq => "==",
            OpSym::Ne => "!=",
            OpSym::In => "in",
            OpSym::NotIn => "not in",
            OpSym::Add => "+",
            OpSym::Sub => "-",
            OpSym::Mul => "*",
            OpSym::Div => "/",
            OpSym::Mod => "%",
            OpSym::Pow => "^",
            OpSym::And => "and",
            OpSym::Or => "or",
            OpSym::Not => "not",
            OpSym::Coalesce => "??",
        }
    }

    pub(crate) fn from_operator(operator: Operator) -> Option<Self> {
        match operator {
            Operator::Arithmetic(a) => Some(match a {
                ArithmeticOperator::Add => OpSym::Add,
                ArithmeticOperator::Subtract => OpSym::Sub,
                ArithmeticOperator::Multiply => OpSym::Mul,
                ArithmeticOperator::Divide => OpSym::Div,
                ArithmeticOperator::Modulus => OpSym::Mod,
                ArithmeticOperator::Power => OpSym::Pow,
            }),
            Operator::Logical(l) => Some(match l {
                LogicalOperator::And => OpSym::And,
                LogicalOperator::Or => OpSym::Or,
                LogicalOperator::Not => OpSym::Not,
                LogicalOperator::NullishCoalescing => OpSym::Coalesce,
            }),
            Operator::Comparison(c) => Some(match c {
                ComparisonOperator::Equal => OpSym::Eq,
                ComparisonOperator::NotEqual => OpSym::Ne,
                ComparisonOperator::LessThan => OpSym::Lt,
                ComparisonOperator::GreaterThan => OpSym::Gt,
                ComparisonOperator::LessThanOrEqual => OpSym::Lte,
                ComparisonOperator::GreaterThanOrEqual => OpSym::Gte,
                ComparisonOperator::In => OpSym::In,
                ComparisonOperator::NotIn => OpSym::NotIn,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WordSym {
    If,
    Then,
    Otherwise,
    In,
    Where,
    Has,
    RangeAnd,
}

use crate::functions::{FunctionKind, MethodKind};
use crate::lexer::Bracket;
use rust_decimal::Decimal;
use std::sync::Arc;
use strum_macros::Display;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchFastTarget {
    Root,
    String(Arc<str>),
    Number(u32),
}

/// Machine code interpreted by VM
#[derive(Debug, PartialEq, Eq, Clone, Display)]
pub enum Opcode {
    PushNull,
    PushBool(bool),
    PushString(Arc<str>),
    PushNumber(Decimal),
    Pop,
    Flatten,
    Join,
    Fetch,
    FetchRootEnv,
    FetchEnv(Arc<str>),
    FetchFast(Vec<FetchFastTarget>),
    Negate,
    Not,
    Equal,
    Jump(Jump, u32),
    In,
    Compare(Compare),
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Exponent,
    Slice,
    Array,
    Object,
    Len,
    IncrementIt,
    IncrementCount,
    GetCount,
    GetLen,
    Pointer,
    Begin,
    End,
    CallFunction {
        kind: FunctionKind,
        arg_count: u32,
    },
    CallMethod {
        kind: MethodKind,
        arg_count: u32,
    },
    Interval {
        left_bracket: Bracket,
        right_bracket: Bracket,
    },
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display)]
pub enum Jump {
    Forward,
    Backward,
    IfTrue,
    IfFalse,
    IfNotNull,
    IfEnd,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Display)]
pub enum Compare {
    More,
    Less,
    MoreOrEqual,
    LessOrEqual,
}

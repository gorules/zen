use crate::vm::Variable;

/// Machine code interpreted by VM
#[derive(Debug, Clone)]
pub enum Opcode<'a> {
    Push(Variable<'a>),
    Pop,
    Rot,
    Fetch,
    FetchEnv(&'a str),
    Negate,
    Not,
    Equal,
    Jump(usize),
    JumpIfTrue(usize),
    JumpIfFalse(usize),
    JumpIfEnd(usize),
    JumpBackward(usize),
    In,
    Less,
    More,
    LessOrEqual,
    MoreOrEqual,
    Abs,
    Average,
    Median,
    Mode,
    Min,
    Max,
    Round,
    Floor,
    Ceil,
    Sum,
    Random,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Exponent,
    Interval {
        left_bracket: &'a str,
        right_bracket: &'a str,
    },
    Contains,
    DateFunction(&'a str),
    DateManipulation(&'a str),
    Uppercase,
    Lowercase,
    StartsWith,
    EndsWith,
    Matches,
    Extract,
    Slice,
    Array,
    Len,
    ParseDateTime,
    ParseTime,
    ParseDuration,
    IncrementIt,
    IncrementCount,
    GetCount,
    GetLen,
    Pointer,
    Begin,
    End,
    Flatten,
    TypeConversion(TypeConversionKind),
    TypeCheck(TypeCheckKind),
}

/// Metadata for TypeConversion Opcode
#[derive(Debug, Clone)]
pub enum TypeConversionKind {
    Number,
    String,
    Bool,
}

/// Metadata for TypeCheck Opcode
#[derive(Debug, Clone)]
pub enum TypeCheckKind {
    Numeric,
}

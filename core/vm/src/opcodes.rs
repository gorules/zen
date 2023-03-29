use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use bumpalo::Bump;
use hashbrown::hash_map::DefaultHashBuilder;
use hashbrown::BumpWrapper;

use rust_decimal::Decimal;
use serde_json::{Map, Number, Value};

#[derive(Debug)]
pub enum Variable<'a> {
    Null,
    Bool(bool),
    Int(i64),
    Number(Decimal),
    String(&'a str),
    Array(&'a [&'a Variable<'a>]),
    Object(hashbrown::HashMap<&'a str, &'a Variable<'a>, DefaultHashBuilder, BumpWrapper<'a>>),
    Interval {
        left_bracket: &'a str,
        right_bracket: &'a str,
        left: &'a Variable<'a>,
        right: &'a Variable<'a>,
    },
}

impl<'a> Display for Variable<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl<'a> Clone for Variable<'a> {
    fn clone(&self) -> Self {
        match self {
            Variable::Null => Variable::Null,
            Variable::Bool(b) => Variable::Bool((*b).clone()),
            Variable::Int(i) => Variable::Int((*i).clone()),
            Variable::Number(n) => Variable::Number(n.clone()),
            Variable::String(s) => Variable::String(s.clone()),
            Variable::Array(a) => Variable::Array(a.clone()),
            Variable::Object(o) => Variable::Object(o.clone()),
            Variable::Interval {
                left_bracket,
                right_bracket,
                left,
                right,
            } => Variable::Interval {
                left_bracket: left_bracket.clone(),
                right_bracket: right_bracket.clone(),
                left: left.clone(),
                right: right.clone(),
            },
        }
    }
}

impl<'a> Variable<'a> {
    pub fn empty_object_in(bump: &'a Bump) -> Self {
        Variable::Object(hashbrown::HashMap::new_in(BumpWrapper(bump)))
    }

    pub fn from_serde(v: &Value, bump: &'a Bump) -> Self {
        match v {
            Value::String(str) => Variable::String(bump.alloc_str(str)),
            Value::Number(f) => {
                Variable::Number(Decimal::from_str_exact(f.to_string().as_str()).unwrap())
            }
            Value::Bool(b) => Variable::Bool((*b).clone()),
            Value::Array(v) => {
                let mut arr: Vec<&'a Variable> = Vec::with_capacity(v.len());
                for i in v {
                    arr.push(bump.alloc(Variable::from_serde(i, bump)));
                }

                Variable::Array(bump.alloc_slice_copy(arr.as_slice()))
            }
            Value::Object(bt) => {
                let mut tree: hashbrown::HashMap<&'a str, &'a Variable, _, _> =
                    hashbrown::HashMap::new_in(BumpWrapper(bump));
                for k in bt.keys() {
                    let v = bt.get(k).unwrap();
                    tree.insert(
                        bump.alloc_str(k.as_str()),
                        bump.alloc(Variable::from_serde(v, bump)),
                    );
                }

                Variable::Object(tree)
            }
            Value::Null => Variable::Null,
        }
    }
}

#[derive(Debug)]
pub enum Opcode<'a> {
    Push(Variable<'a>),
    Pop,
    Rot,
    Fetch,
    FetchField(&'a str),
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
    DateManipulation(&'a str),
    Uppercase,
    Lowercase,
    StartsWith,
    EndsWith,
    Slice,
    Array,
    Len,
    ParseTime,
    ParseDuration,
    IncrementIt,
    IncrementCount,
    GetCount,
    GetLen,
    Pointer,
    Begin,
    End,
}

impl<'a> Display for Opcode<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum ExecResult {
    Null,
    Bool(bool),
    Number(Decimal),
    String(String),
    Array(Vec<ExecResult>),
    Object(HashMap<String, ExecResult>),
}

impl TryFrom<&Variable<'_>> for ExecResult {
    type Error = ();

    fn try_from(value: &Variable) -> Result<Self, Self::Error> {
        match value {
            Variable::Null => Ok(ExecResult::Null),
            Variable::Bool(b) => Ok(ExecResult::Bool((*b).clone())),
            Variable::Number(n) => Ok(ExecResult::Number(*n)),
            Variable::String(s) => Ok(ExecResult::String(s.to_string())),
            Variable::Array(arr) => {
                let mut v = Vec::<ExecResult>::with_capacity(arr.len());
                for i in *arr {
                    v.push(ExecResult::try_from(*i)?)
                }

                Ok(ExecResult::Array(v))
            }
            Variable::Object(obj) => {
                let mut t = HashMap::new();

                for k in obj.keys() {
                    let v = *obj.get(k).ok_or_else(|| ())?;
                    t.insert(k.to_string(), ExecResult::try_from(v)?);
                }

                Ok(ExecResult::Object(t))
            }
            _ => Err(()),
        }
    }
}

impl ExecResult {
    pub fn to_variable<'a>(&self, bump: &'a Bump) -> Result<&'a Variable<'a>, ()> {
        match self {
            ExecResult::Null => Ok(bump.alloc(Variable::Null)),
            ExecResult::Bool(b) => Ok(bump.alloc(Variable::Bool((*b).clone()))),
            ExecResult::Number(n) => Ok(bump.alloc(Variable::Number(*n))),
            ExecResult::String(str) => Ok(bump.alloc(Variable::String(bump.alloc_str(str)))),
            ExecResult::Array(arr) => {
                let mut v = Vec::<&'a Variable<'a>>::with_capacity(arr.len());
                for i in arr {
                    v.push(i.to_variable(bump)?)
                }

                Ok(bump.alloc(Variable::Array(bump.alloc_slice_copy(v.as_slice()))))
            }
            ExecResult::Object(obj) => {
                let mut t = hashbrown::HashMap::<&'a str, _, _, _>::new_in(BumpWrapper(bump));

                for k in obj.keys() {
                    let v = obj.get(k).ok_or_else(|| ())?;
                    t.insert(bump.alloc_str(k), v.to_variable(bump)?);
                }

                Ok(bump.alloc(Variable::Object(t)))
            }
        }
    }

    pub fn to_value(&self) -> Result<Value, ()> {
        match self {
            ExecResult::Null => Ok(Value::Null),
            ExecResult::Bool(b) => Ok(Value::Bool((*b).clone())),
            ExecResult::Number(n) => Ok(Value::Number(
                Number::from_str(n.to_string().as_str()).map_err(|_| ())?,
            )),
            ExecResult::String(s) => Ok(Value::String(s.clone())),
            ExecResult::Array(arr) => {
                let mut v = Vec::<Value>::with_capacity(arr.len());
                for i in arr {
                    v.push(i.to_value()?)
                }

                Ok(Value::Array(v))
            }
            ExecResult::Object(obj) => {
                let mut t = Map::new();

                for k in obj.keys() {
                    let v = obj.get(k).ok_or_else(|| ())?;
                    t.insert(k.to_string(), v.to_value()?);
                }

                Ok(Value::Object(t))
            }
        }
    }

    pub fn bool(&self) -> Result<bool, ()> {
        match self {
            ExecResult::Bool(b) => Ok((*b).clone()),
            _ => Err(()),
        }
    }
}

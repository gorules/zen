use std::str::FromStr;

use bumpalo::Bump;
use chrono::NaiveDateTime;
use hashbrown::hash_map::DefaultHashBuilder;
use hashbrown::BumpWrapper;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{Map, Number, Value};

use crate::helpers::date_time;
use crate::vm::VMError;
use crate::vm::VMError::{OpcodeErr, ParseDateTimeErr};

#[derive(Debug)]
pub enum Variable<'a> {
    Null,
    Bool(bool),
    Number(Decimal),
    String(&'a str),
    Array(&'a [&'a Variable<'a>]),
    Object(hashbrown::HashMap<&'a str, &'a Variable<'a>, DefaultHashBuilder, BumpWrapper<'a>>),
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
            Value::Bool(b) => Variable::Bool(*b),
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

    pub(crate) fn as_str(&self) -> Option<&'a str> {
        match self {
            Variable::String(str) => Some(str),
            _ => None,
        }
    }

    pub(crate) fn type_name(&self) -> &str {
        match self {
            Variable::Null => "null",
            Variable::Bool(_) => "bool",
            Variable::Number(_) => "number",
            Variable::String(_) => "string",
            Variable::Array(_) => "array",
            Variable::Object(_) => "object",
        }
    }
}

#[derive(Debug)]
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

#[derive(Debug)]
pub enum TypeConversionKind {
    Number,
    String,
}

#[derive(Debug)]
pub enum TypeCheckKind {
    Numeric,
}

impl TryFrom<&Variable<'_>> for Value {
    type Error = ();

    fn try_from(value: &Variable<'_>) -> Result<Self, Self::Error> {
        match value {
            Variable::Null => Ok(Value::Null),
            Variable::Bool(b) => Ok(Value::Bool(*b)),
            Variable::Number(n) => Ok(Value::Number(
                Number::from_str(n.normalize().to_string().as_str()).map_err(|_| ())?,
            )),
            Variable::String(s) => Ok(Value::String(s.to_string())),
            Variable::Array(arr) => {
                let mut v = Vec::<Value>::with_capacity(arr.len());
                for i in *arr {
                    v.push(Value::try_from(*i)?)
                }

                Ok(Value::Array(v))
            }
            Variable::Object(obj) => {
                let mut t = Map::new();

                for k in obj.keys() {
                    let v = *obj.get(k).ok_or(())?;
                    t.insert(k.to_string(), Value::try_from(v)?);
                }

                Ok(Value::Object(t))
            }
        }
    }
}

impl TryFrom<&Variable<'_>> for NaiveDateTime {
    type Error = VMError;

    fn try_from(value: &Variable<'_>) -> Result<Self, Self::Error> {
        match value {
            Variable::String(a) => date_time(a),
            Variable::Number(a) => NaiveDateTime::from_timestamp_opt(
                a.to_i64().ok_or_else(|| OpcodeErr {
                    opcode: "DateManipulation".into(),
                    message: "Failed to extract date".into(),
                })?,
                0,
            )
            .ok_or_else(|| ParseDateTimeErr {
                timestamp: a.to_string(),
            }),
            _ => Err(OpcodeErr {
                opcode: "DateManipulation".into(),
                message: "Unsupported type".into(),
            }),
        }
    }
}

pub(crate) struct IntervalObject<'a> {
    pub(crate) left_bracket: &'a str,
    pub(crate) right_bracket: &'a str,
    pub(crate) left: &'a Variable<'a>,
    pub(crate) right: &'a Variable<'a>,
}

impl<'a> IntervalObject<'a> {
    pub(crate) fn cast_to_object(&self, bump: &'a Bump) -> Variable<'a> {
        let mut tree: hashbrown::HashMap<&'a str, &'a Variable, _, _> =
            hashbrown::HashMap::new_in(BumpWrapper(bump));

        tree.insert("_symbol", &Variable::String("Interval"));
        tree.insert(
            "left_bracket",
            bump.alloc(Variable::String(self.left_bracket)),
        );
        tree.insert(
            "right_bracket",
            bump.alloc(Variable::String(self.right_bracket)),
        );
        tree.insert("left", self.left);
        tree.insert("right", self.right);

        Variable::Object(tree)
    }

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

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
            _ => Err(()),
        }
    }
}

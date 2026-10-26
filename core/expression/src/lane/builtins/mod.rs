mod arrays;
mod convert;
pub(crate) mod dates;
mod functions;
mod legacy;
mod math;
mod text;

pub(crate) use dates::Dates;
pub(crate) use functions::Builtins;
pub(crate) use text::{Text, TextKernel};

use crate::lane::columns::{Column, Values};
use crate::lane::date::Date;
use crate::lane::date::DynamicVariableExt;
use crate::variable::Variable;
use rust_decimal::Decimal;
use std::borrow::Cow;
use zen_types::rccell::RcCell;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ListView<'a> {
    pub child: &'a Column<'a>,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Arg<'a> {
    Null,
    Num(Decimal),
    Bool(bool),
    Str(&'a str),
    Date(Date),
    Var(&'a Variable),
    List(ListView<'a>),
}

impl<'a> Arg<'a> {
    pub(crate) fn of(v: &'a Variable) -> Self {
        match v {
            Variable::Null => Arg::Null,
            Variable::Number(n) => Arg::Num(*n),
            Variable::Bool(b) => Arg::Bool(*b),
            Variable::String(s) => Arg::Str(s.as_str()),
            Variable::Dynamic(d) => match d.as_date() {
                Some(date) if !Date::sourced(v) => Arg::Date(date),
                _ => Arg::Var(v),
            },
            v => Arg::Var(v),
        }
    }

    pub(crate) fn date(&self) -> Option<Date> {
        match self {
            Arg::Date(d) => Some(*d),
            Arg::Var(Variable::Dynamic(d)) => d.as_date(),
            _ => None,
        }
    }

    pub(crate) fn text(&self) -> Option<Cow<'a, str>> {
        match self {
            Arg::Str(s) => Some(Cow::Borrowed(s)),
            Arg::Date(d) => d.rendered().map(Cow::Owned),
            Arg::Var(v @ Variable::Dynamic(_)) => v.as_str().map(Cow::Borrowed),
            _ => None,
        }
    }

    pub(crate) fn coerce_date(&self) -> Option<Date> {
        match self {
            Arg::Str(s) => Some(Date::from_text(s)),
            other => other.date(),
        }
    }

    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Arg::Null => "null",
            Arg::Num(_) => "number",
            Arg::Bool(_) => "bool",
            Arg::Str(_) => "string",
            Arg::Date(_) => "date",
            Arg::Var(v) => v.type_name(),
            Arg::List(_) => "array",
        }
    }

    pub(crate) fn variable(&self) -> Variable {
        match self {
            Arg::Null => Variable::Null,
            Arg::Num(n) => Variable::Number(*n),
            Arg::Bool(b) => Variable::Bool(*b),
            Arg::Str(s) => Variable::String((*s).into()),
            Arg::Date(d) => d.variable(),
            Arg::Var(v) => (*v).clone(),
            Arg::List(l) => {
                Variable::from_array((l.start..l.end).map(|i| l.child.variable(i)).collect())
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Array<'a> {
    Var(&'a RcCell<Vec<Variable>>),
    List(ListView<'a>),
}

impl<'a> Array<'a> {
    pub(crate) fn len(&self) -> usize {
        match self {
            Array::Var(a) => a.borrow().len(),
            Array::List(l) => l.end - l.start,
        }
    }

    pub(crate) fn numbers(&self) -> Option<Cow<'a, [Decimal]>> {
        match self {
            Array::List(l) => match l.child.values {
                Values::Dec(values) if (l.start..l.end).all(|i| l.child.valid(i)) => {
                    values.get(l.start..l.end).map(Cow::Borrowed)
                }
                _ => (l.start..l.end)
                    .map(|i| l.child.valid(i).then(|| l.child.number(i)).flatten())
                    .collect::<Option<Vec<_>>>()
                    .map(Cow::Owned),
            },
            Array::Var(a) => a
                .borrow()
                .iter()
                .map(Variable::as_number)
                .collect::<Option<Vec<_>>>()
                .map(Cow::Owned),
        }
    }

    pub(crate) fn variables(&self) -> Vec<Variable> {
        match self {
            Array::Var(a) => a.borrow().clone(),
            Array::List(l) => (l.start..l.end).map(|i| l.child.variable(i)).collect(),
        }
    }

    pub(crate) fn with<T>(&self, f: impl FnOnce(&[Variable]) -> T) -> T {
        match self {
            Array::Var(a) => f(&a.borrow()),
            Array::List(_) => f(&self.variables()),
        }
    }

    pub(crate) fn any(&self, mut f: impl FnMut(Arg) -> bool) -> bool {
        match self {
            Array::Var(a) => a.borrow().iter().any(|v| f(Arg::of(v))),
            Array::List(l) => (l.start..l.end).any(|i| {
                let v = l.child.variable(i);
                f(Arg::of(&v))
            }),
        }
    }
}

pub(crate) trait FromArg<'a>: Sized {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self>;
}

impl<'a> FromArg<'a> for &'a str {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        match a? {
            Arg::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for Decimal {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        match a? {
            Arg::Num(n) => Some(n),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for bool {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        match a? {
            Arg::Bool(b) => Some(b),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for Array<'a> {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        match a? {
            Arg::Var(Variable::Array(a)) => Some(Array::Var(a)),
            Arg::List(l) => Some(Array::List(l)),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for Arg<'a> {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        a
    }
}

impl<'a, T: FromArg<'a>> FromArg<'a> for Option<T> {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        match a {
            None => Some(None),
            Some(a) => T::from_arg(Some(a)).map(Some),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Out {
    Num(Decimal),
    Bool(bool),
    Str(zen_types::symbol::Symbol),
    Date(Date),
    Var(Variable),
}

impl Out {
    pub(crate) fn variable(self) -> Variable {
        match self {
            Out::Num(n) => Variable::Number(n),
            Out::Bool(b) => Variable::Bool(b),
            Out::Str(s) => Variable::String(s),
            Out::Date(d) => d.variable(),
            Out::Var(v) => v,
        }
    }
}

impl From<Decimal> for Out {
    fn from(n: Decimal) -> Self {
        Out::Num(n)
    }
}

impl From<usize> for Out {
    fn from(n: usize) -> Self {
        Out::Num(n.into())
    }
}

impl From<bool> for Out {
    fn from(b: bool) -> Self {
        Out::Bool(b)
    }
}

impl From<String> for Out {
    fn from(s: String) -> Self {
        Out::Str(zen_types::symbol::Symbol::from(s.as_str()))
    }
}

impl From<&str> for Out {
    fn from(s: &str) -> Self {
        Out::Str(zen_types::symbol::Symbol::from(s))
    }
}

impl From<Date> for Out {
    fn from(d: Date) -> Self {
        Out::Date(d)
    }
}

impl From<Variable> for Out {
    fn from(v: Variable) -> Self {
        Out::Var(v)
    }
}

pub(crate) type Fail = String;

pub(crate) type Outcome = Option<Result<Out, Fail>>;

pub(crate) type Overload = for<'a> fn(&[Arg<'a>]) -> Outcome;

pub(crate) struct Apply;

impl Apply {
    #[inline(always)]
    pub(crate) fn one<'a, A, R>(args: &[Arg<'a>], f: impl FnOnce(A) -> Result<R, Fail>) -> Outcome
    where
        A: FromArg<'a>,
        R: Into<Out>,
    {
        let a = A::from_arg(args.first().copied())?;
        Some(f(a).map(Into::into))
    }

    #[inline(always)]
    pub(crate) fn two<'a, A, B, R>(
        args: &[Arg<'a>],
        f: impl FnOnce(A, B) -> Result<R, Fail>,
    ) -> Outcome
    where
        A: FromArg<'a>,
        B: FromArg<'a>,
        R: Into<Out>,
    {
        let a = A::from_arg(args.first().copied())?;
        let b = B::from_arg(args.get(1).copied())?;
        Some(f(a, b).map(Into::into))
    }
}

pub(crate) struct Params;

impl Params {
    pub(crate) fn fail(args: &[Arg], wants: &[(&str, bool)]) -> Fail {
        for (pos, (want, optional)) in wants.iter().enumerate() {
            let matches = |a: &Arg| match *want {
                "string" => a.text().is_some(),
                "number" => matches!(a, Arg::Num(_)),
                "bool" => matches!(a, Arg::Bool(_)),
                "array" => matches!(a, Arg::Var(Variable::Array(_)) | Arg::List(_)),
                "object" => matches!(a, Arg::Var(Variable::Object(_))),
                _ => true,
            };
            match (args.get(pos), optional) {
                (None, true) => {}
                (None, false) => {
                    return format!("Argument on {pos} position is not a valid {want}")
                }
                (Some(a), _) if !matches(a) => return format!("Argument on {pos} is not a {want}"),
                (Some(_), _) => {}
            }
        }
        "No overload matches provided arguments".to_string()
    }
}

pub(crate) struct Builtin {
    pub(crate) overloads: &'static [Overload],
    pub(crate) fail: fn(&[Arg]) -> Fail,
}

impl<'a> FromArg<'a> for &'a Variable {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        match a? {
            Arg::Var(v) => Some(v),
            _ => None,
        }
    }
}

impl<'a> FromArg<'a> for Date {
    fn from_arg(a: Option<Arg<'a>>) -> Option<Self> {
        a?.date()
    }
}

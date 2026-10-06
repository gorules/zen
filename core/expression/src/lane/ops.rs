use crate::compiler::{Compare, FetchFastTarget};
use crate::lane::builtins::{Arg, Builtins, Out, Text};
use crate::lane::date::{Date, DynamicVariableExt};
use crate::lane::interval::{Interval, IntervalData};
use crate::lexer::Bracket;
use crate::scope::Scope;
use crate::variable::Variable;
use crate::variable::Variable::*;
use crate::vm::VMError;
use crate::vm::VMError::*;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::{Decimal, MathematicalOps};
use std::rc::Rc;
use std::string::String as StdString;
use zen_types::symbol::Symbol;

pub(crate) struct Ops;

type VMResult<T> = Result<T, VMError>;

impl Ops {
    pub(crate) fn error(opcode: &str, message: impl Into<StdString>) -> VMError {
        OpcodeErr {
            opcode: opcode.into(),
            message: message.into(),
        }
    }

    pub(crate) fn unsupported(opcode: &str) -> VMError {
        Self::error(opcode, "Unsupported type")
    }

    pub(crate) fn fetch(a: Variable, b: Variable) -> VMResult<Variable> {
        match (Date::textual(a), Date::textual(b)) {
            (Object(o), String(s)) => Ok(o.borrow().get_str(s.as_str()).cloned().unwrap_or(Null)),
            (Array(a), Number(n)) => {
                let index = n
                    .to_usize()
                    .ok_or_else(|| Self::error("Fetch", "Failed to convert to usize"))?;
                Ok(a.borrow().get(index).cloned().unwrap_or(Null))
            }
            (String(str), Number(n)) => {
                let index = n
                    .to_usize()
                    .ok_or_else(|| Self::error("Fetch", "Failed to convert to usize"))?;
                let slice = index.checked_add(1).and_then(|end| str.get(index..end));
                Ok(slice.map_or(Null, |s| String(s.into())))
            }
            _ => Ok(Null),
        }
    }

    pub(crate) fn step(
        v: Variable,
        target: &FetchFastTarget,
        root: &Scope,
        env: &Scope,
    ) -> Variable {
        match target {
            FetchFastTarget::Root => root.materialize(),
            FetchFastTarget::Begin => env.materialize(),
            FetchFastTarget::String(key) => match v {
                Object(obj) => obj.borrow().get_str(key).cloned().unwrap_or(Null),
                _ => Null,
            },
            FetchFastTarget::Number(num) => match Date::textual(v) {
                Array(arr) => arr.borrow().get(*num as usize).cloned().unwrap_or(Null),
                String(str) => {
                    let index = *num as usize;
                    str.get(index..index + 1)
                        .map(|slice| String(slice.into()))
                        .unwrap_or(Null)
                }
                _ => Null,
            },
        }
    }

    pub(crate) fn fetch_fast(path: &[FetchFastTarget], root: &Scope, env: &Scope) -> Variable {
        let mut steps = path.iter();
        let first = match steps.next() {
            Some(FetchFastTarget::Root) => root.materialize(),
            Some(FetchFastTarget::Begin) => match steps.clone().next() {
                Some(FetchFastTarget::String(key)) => {
                    steps.next();
                    env.get_str(key).unwrap_or(Null)
                }
                _ => env.materialize(),
            },
            _ => Null,
        };

        steps.fold(first, |v, p| Self::step(v, p, root, env))
    }

    pub(crate) fn negate(a: Variable) -> VMResult<Variable> {
        match a {
            Number(n) => Ok(Number(-n)),
            _ => Err(Self::unsupported("Negate")),
        }
    }

    pub(crate) fn not(a: Variable) -> VMResult<Variable> {
        match a {
            Bool(b) => Ok(Bool(!b)),
            _ => Err(Self::unsupported("Not")),
        }
    }

    pub(crate) fn equal(a: &Variable, b: &Variable) -> bool {
        match (a, b) {
            (Number(a), Number(b)) => Self::same_number(a, b),
            (Bool(a), Bool(b)) => a == b,
            (String(a), String(b)) => a == b,
            (Null, Null) => true,
            (Dynamic(a), Dynamic(b)) => {
                let (a, b) = (a.as_date(), b.as_date());
                a.is_some() && b.is_some() && a == b
            }
            (Dynamic(a), b @ String(_)) | (b @ String(_), Dynamic(a)) => {
                a.as_date().is_some_and(|a| a.matches(b))
            }
            _ => false,
        }
    }

    pub(crate) fn truthy(a: &Variable, opcode: &str) -> VMResult<bool> {
        match a {
            Bool(b) => Ok(*b),
            _ => Err(Self::unsupported(opcode)),
        }
    }

    pub(crate) fn membership(a: Variable, b: &Variable) -> VMResult<bool> {
        let a = match b {
            Object(_) => Date::textual(a),
            _ => a,
        };
        match (a, b) {
            (Number(a), Array(b)) => Ok(b
                .borrow()
                .iter()
                .any(|b| matches!(b, Number(b) if Self::same_number(&a, b)))),
            (Number(v), Dynamic(d)) => {
                let Some(i) = d.as_any().downcast_ref::<Interval>() else {
                    return Err(Self::unsupported("In"));
                };
                i.includes(IntervalData::Number(v))
                    .map_err(|err| Self::error("In", err.to_string()))
            }
            (Dynamic(d), Dynamic(i)) => {
                let Some(d) = d.as_date() else {
                    return Err(Self::unsupported("In"));
                };
                let Some(i) = i.as_any().downcast_ref::<Interval>() else {
                    return Err(Self::unsupported("In"));
                };
                i.includes(IntervalData::Date(d))
                    .map_err(|err| Self::error("In", err.to_string()))
            }
            (Dynamic(a), Array(arr)) => {
                let Some(a) = a.as_date() else {
                    return Err(Self::unsupported("In"));
                };
                Ok(arr.borrow().iter().any(|b| a.matches(b)))
            }
            (String(a), Array(b)) => {
                let text = String(a.clone());
                Ok(b.borrow().iter().any(|b| match b {
                    String(b) => &a == b,
                    Dynamic(d) => d.as_date().is_some_and(|d| d.matches(&text)),
                    _ => false,
                }))
            }
            (String(a), Object(b)) => Ok(b.borrow().contains_key_str(a.as_str())),
            (Bool(a), Array(b)) => Ok(b.borrow().iter().any(|b| matches!(b, Bool(b) if a == *b))),
            (Null, Array(b)) => Ok(b.borrow().iter().any(|b| matches!(b, Null))),
            _ => Err(Self::unsupported("In")),
        }
    }

    #[inline]
    pub(crate) fn order(a: &Decimal, b: &Decimal) -> std::cmp::Ordering {
        match a.scale() == b.scale() {
            true => a.mantissa().cmp(&b.mantissa()),
            false => a.cmp(b),
        }
    }

    #[inline]
    pub(crate) fn same_number(a: &Decimal, b: &Decimal) -> bool {
        Self::order(a, b).is_eq()
    }

    #[inline]
    pub(crate) fn ordered_number(a: &Decimal, b: &Decimal, comparison: Compare) -> bool {
        let o = Self::order(a, b);
        match comparison {
            Compare::More => o.is_gt(),
            Compare::MoreOrEqual => o.is_ge(),
            Compare::Less => o.is_lt(),
            Compare::LessOrEqual => o.is_le(),
        }
    }

    pub(crate) fn ordered<T: Ord>(a: &T, b: &T, comparison: Compare) -> bool {
        match comparison {
            Compare::More => a > b,
            Compare::MoreOrEqual => a >= b,
            Compare::Less => a < b,
            Compare::LessOrEqual => a <= b,
        }
    }

    pub(crate) fn compared(a: &Variable, b: &Variable, comparison: Compare) -> Option<bool> {
        match (a, b) {
            (Number(a), Number(b)) => Some(Self::ordered_number(a, b, comparison)),
            (Dynamic(_), _) | (_, Dynamic(_)) => Self::compare(a, b, comparison).ok(),
            _ => None,
        }
    }

    pub(crate) fn compare(a: &Variable, b: &Variable, comparison: Compare) -> VMResult<bool> {
        match (a, b) {
            (Number(a), Number(b)) => Ok(Self::ordered_number(a, b, comparison)),
            (Dynamic(a), Dynamic(b)) => match (a.as_date(), b.as_date()) {
                (Some(a), Some(b)) => Ok(Self::ordered(&a, &b, comparison)),
                _ => Err(Self::unsupported("Compare")),
            },
            (Dynamic(_), String(_)) | (String(_), Dynamic(_)) => {
                let valid = |date: &Date| date.is_valid();
                match (
                    Date::coerce(a).filter(valid),
                    Date::coerce(b).filter(valid),
                ) {
                    (Some(a), Some(b)) => Ok(Self::ordered(&a, &b, comparison)),
                    _ => Err(Self::unsupported("Compare")),
                }
            }
            _ => Err(Self::unsupported("Compare")),
        }
    }

    pub(crate) fn add(a: Variable, b: Variable) -> VMResult<Variable> {
        match (Date::textual(a), Date::textual(b)) {
            (Number(a), Number(b)) => a
                .checked_add(b)
                .map(Number)
                .ok_or_else(|| Self::error("Add", "Number overflow")),
            (String(a), String(b)) => {
                let mut c = StdString::with_capacity(a.len() + b.len());
                c.push_str(a.as_ref());
                c.push_str(b.as_ref());
                Ok(String(c.as_str().into()))
            }
            _ => Err(Self::unsupported("Add")),
        }
    }

    pub(crate) fn subtract(a: Variable, b: Variable) -> VMResult<Variable> {
        match (a, b) {
            (Number(a), Number(b)) => a
                .checked_sub(b)
                .map(Number)
                .ok_or_else(|| Self::error("Subtract", "Number overflow")),
            _ => Err(Self::unsupported("Subtract")),
        }
    }

    pub(crate) fn multiply(a: Variable, b: Variable) -> VMResult<Variable> {
        match (a, b) {
            (Number(a), Number(b)) => a
                .checked_mul(b)
                .map(Number)
                .ok_or_else(|| Self::error("Multiply", "Number overflow")),
            _ => Err(Self::unsupported("Multiply")),
        }
    }

    pub(crate) fn divide(a: Variable, b: Variable) -> VMResult<Variable> {
        match (a, b) {
            (Number(a), Number(b)) => Ok(a.checked_div(b).map_or(Null, Number)),
            _ => Err(Self::unsupported("Divide")),
        }
    }

    pub(crate) fn modulo(a: Variable, b: Variable) -> VMResult<Variable> {
        match (a, b) {
            (Number(a), Number(b)) => Ok(a.checked_rem(b).map_or(Null, Number)),
            _ => Err(Self::unsupported("Modulo")),
        }
    }

    pub(crate) fn exponent(a: Variable, b: Variable) -> VMResult<Variable> {
        match (a, b) {
            (Number(a), Number(b)) => a
                .checked_powd(b)
                .or_else(|| Decimal::from_f64(a.to_f64()?.powf(b.to_f64()?)))
                .map(Number)
                .ok_or_else(|| Self::error("Exponent", "Failed to calculate exponent")),
            _ => Err(Self::unsupported("Exponent")),
        }
    }

    pub(crate) fn interval(
        a: &Variable,
        b: &Variable,
        left_bracket: Bracket,
        right_bracket: Bracket,
    ) -> VMResult<Variable> {
        let (left, right) = match (a, b) {
            (Number(a), Number(b)) => (IntervalData::Number(*a), IntervalData::Number(*b)),
            (Dynamic(a), Dynamic(b)) => match (a.as_date(), b.as_date()) {
                (Some(a), Some(b)) => {
                    (IntervalData::Date(a), IntervalData::Date(b))
                }
                _ => return Err(Self::unsupported("Interval")),
            },
            _ => return Err(Self::unsupported("Interval")),
        };

        Ok(Dynamic(Rc::new(Interval {
            left_bracket,
            right_bracket,
            left,
            right,
        })))
    }

    pub(crate) fn slice(current: Variable, to: Variable, from: Variable) -> VMResult<Variable> {
        let (Number(f), Number(t)) = (from, to) else {
            return Err(Self::unsupported("Slice"));
        };

        let from = f
            .to_usize()
            .ok_or_else(|| Self::error("Slice", "Failed to get range from"))?;
        let to = t
            .to_usize()
            .ok_or_else(|| Self::error("Slice", "Failed to get range to"))?;

        match Date::textual(current) {
            Array(a) => {
                let arr = a.borrow();
                let slice = arr
                    .get(from..=to)
                    .ok_or_else(|| Self::error("Slice", "Index out of range"))?;
                Ok(Variable::from_array(slice.to_vec()))
            }
            String(s) => {
                let slice = s
                    .get(from..=to)
                    .ok_or_else(|| Self::error("Slice", "Index out of range"))?;
                Ok(String(slice.into()))
            }
            _ => Err(Self::unsupported("Slice")),
        }
    }

    pub(crate) fn object_key(key: Variable) -> VMResult<Symbol> {
        match key {
            String(key) => Ok(Symbol::from(key.as_str())),
            _ => Err(Self::error("Object", "Unexpected key value")),
        }
    }

    pub(crate) fn assigned_key(key: Variable) -> VMResult<Symbol> {
        match key {
            String(key) => Ok(key),
            _ => Err(Self::error("AssignedObjectStep", "Unexpected key value")),
        }
    }

    pub(crate) fn assign(
        env: &mut Scope,
        assigned: &Variable,
        key: &Symbol,
        value: Variable,
    ) -> VMResult<()> {
        if !matches!(env.base(), Object(_)) {
            return Err(Self::error(
                "AssignedObjectStep",
                "Failed to mutate existing env",
            ));
        }

        match key.contains('.') {
            false => env.set_local(Symbol::from(key.as_str()), value.clone()),
            true => {
                let Some(new_env) = env
                    .materialize()
                    .dot_insert_detached(key.as_ref(), value.clone())
                else {
                    return Err(Self::error(
                        "AssignedObjectStep",
                        "Failed to mutate existing env",
                    ));
                };
                *env = Scope::new(new_env);
            }
        }

        assigned.dot_insert(key.as_ref(), value);
        Ok(())
    }

    pub(crate) fn len(current: &Variable) -> VMResult<Variable> {
        Builtins::raw(&Text::LEN, &[Arg::of(current)])
            .map(Out::variable)
            .map_err(|message| Self::error("Len", message))
    }

    pub(crate) fn flatten(current: Variable) -> VMResult<Variable> {
        let Array(a) = current else {
            return Err(Self::unsupported("Flatten"));
        };

        let arr = a.borrow();
        let mut flat = Vec::with_capacity(arr.len());
        arr.iter().for_each(|v| match v {
            Array(b) => b.borrow().iter().for_each(|v| flat.push(v.clone())),
            _ => flat.push(v.clone()),
        });

        Ok(Variable::from_array(flat))
    }

    pub(crate) fn elements(list: &Variable) -> VMResult<Variable> {
        match list {
            Array(_) => Ok(list.clone()),
            _ => list
                .dynamic::<Interval>()
                .and_then(|s| s.to_array())
                .map(Variable::from_array)
                .ok_or_else(|| Self::unsupported("Begin")),
        }
    }
}

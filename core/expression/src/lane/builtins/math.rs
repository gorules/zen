use crate::lane::builtins::{Apply, Array, Builtin, Fail, Params};
use crate::lane::date::Date;
use crate::lane::date::DynamicVariableExt;
use crate::variable::Variable;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use std::collections::BTreeMap;

pub(crate) struct Math;

enum Extreme {
    Numbers(Vec<Decimal>),
    Dates(Vec<(Date, Variable)>),
}

impl Math {
    const NUMBER: &'static [(&'static str, bool)] = &[("number", false)];
    const PLACES: &'static [(&'static str, bool)] = &[("number", false), ("number", true)];
    const ARRAY: &'static [(&'static str, bool)] = &[("array", false)];

    fn places(places: Option<Decimal>) -> Result<u32, Fail> {
        match places {
            Some(p) => p
                .to_u32()
                .ok_or_else(|| "Invalid number of decimal places".to_string()),
            None => Ok(0),
        }
    }

    fn numbers(a: &Array) -> Result<Vec<Decimal>, Fail> {
        a.numbers()
            .map(|n| n.into_owned())
            .ok_or_else(|| "Expected a number array".to_string())
    }

    fn total(values: &[Decimal]) -> Result<Decimal, Fail> {
        values
            .iter()
            .try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v))
            .ok_or_else(|| "Number overflow".to_string())
    }

    pub(crate) const ABS: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: Decimal| Ok(a.abs()))],
        fail: |args| Params::fail(args, Self::NUMBER),
    };

    pub(crate) const CEIL: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: Decimal| Ok(a.ceil()))],
        fail: |args| Params::fail(args, Self::NUMBER),
    };

    pub(crate) const FLOOR: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: Decimal| Ok(a.floor()))],
        fail: |args| Params::fail(args, Self::NUMBER),
    };

    pub(crate) const ROUND: Builtin = Builtin {
        overloads: &[|args| {
            Apply::two(args, |a: Decimal, places: Option<Decimal>| {
                Ok(a.round_dp_with_strategy(
                    Self::places(places)?,
                    RoundingStrategy::MidpointAwayFromZero,
                ))
            })
        }],
        fail: |args| Params::fail(args, Self::PLACES),
    };

    pub(crate) const TRUNC: Builtin = Builtin {
        overloads: &[|args| {
            Apply::two(args, |a: Decimal, places: Option<Decimal>| {
                Ok(a.trunc_with_scale(Self::places(places)?))
            })
        }],
        fail: |args| Params::fail(args, Self::PLACES),
    };

    pub(crate) const RAND: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Decimal| {
                let upper = a
                    .round()
                    .to_i64()
                    .ok_or_else(|| "Invalid upper range".to_string())?;
                Ok(Decimal::from(fastrand::i64(0..=upper)))
            })
        }],
        fail: |args| Params::fail(args, Self::NUMBER),
    };

    pub(crate) const SUM: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Array| {
                let numbers = a
                    .numbers()
                    .ok_or_else(|| "Expected a number array".to_string())?;
                Self::total(&numbers)
            })
        }],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    pub(crate) const AVG: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Array| {
                let numbers = a
                    .numbers()
                    .ok_or_else(|| "Expected a number array".to_string())?;
                let sum = Self::total(&numbers)?;
                sum.checked_div(Decimal::from(numbers.len()))
                    .ok_or_else(|| "Empty array".to_string())
            })
        }],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    pub(crate) const MEDIAN: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Array| {
                let mut values = Self::numbers(&a)?;
                values.sort();
                let center = values.len() / 2;
                match values.len() % 2 {
                    1 => values
                        .get(center)
                        .copied()
                        .ok_or_else(|| "Index out of bounds".to_string()),
                    _ => {
                        let left = center
                            .checked_sub(1)
                            .and_then(|i| values.get(i))
                            .ok_or_else(|| "Index out of bounds".to_string())?;
                        let right = values
                            .get(center)
                            .ok_or_else(|| "Index out of bounds".to_string())?;
                        let total = left
                            .checked_add(*right)
                            .ok_or_else(|| "Number overflow".to_string())?;
                        Ok(total / Decimal::TWO)
                    }
                }
            })
        }],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    pub(crate) const MODE: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Array| {
                let mut counts = BTreeMap::new();
                for n in Self::numbers(&a)? {
                    *counts.entry(n).or_insert(0usize) += 1;
                }
                counts
                    .into_iter()
                    .max_by_key(|&(_, count)| count)
                    .map(|(n, _)| n)
                    .ok_or_else(|| "Empty array".to_string())
            })
        }],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    fn extreme(a: &Array) -> Result<Extreme, Fail> {
        a.with(|values| {
            let numeric = values.first().and_then(Variable::as_number).is_some();
            match numeric {
                true => values
                    .iter()
                    .map(Variable::as_number)
                    .collect::<Option<Vec<_>>>()
                    .map(Extreme::Numbers)
                    .ok_or_else(|| "Expected a number array".to_string()),
                false => values
                    .iter()
                    .map(|v| match v {
                        Variable::Dynamic(d) => d.as_date().map(|date| (date, v.clone())),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(Extreme::Dates)
                    .ok_or_else(|| "Expected a number array".to_string()),
            }
        })
    }

    fn pick(a: Array, largest: bool) -> Result<Variable, Fail> {
        let empty = || "Empty array".to_string();
        match (Self::extreme(&a)?, largest) {
            (Extreme::Numbers(n), true) => {
                n.into_iter().max().map(Variable::Number).ok_or_else(empty)
            }
            (Extreme::Numbers(n), false) => {
                n.into_iter().min().map(Variable::Number).ok_or_else(empty)
            }
            (Extreme::Dates(d), true) => d
                .into_iter()
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, original)| original)
                .ok_or_else(empty),
            (Extreme::Dates(d), false) => d
                .into_iter()
                .min_by(|a, b| a.0.cmp(&b.0))
                .map(|(_, original)| original)
                .ok_or_else(empty),
        }
    }

    pub(crate) const MIN: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: Array| Self::pick(a, false))],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    pub(crate) const MAX: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: Array| Self::pick(a, true))],
        fail: |args| Params::fail(args, Self::ARRAY),
    };
}

use crate::lane::builtins::{Apply, Arg, Builtin, Params};
use crate::lane::date::Date;
use crate::variable::Variable;
use chrono_tz::Tz;
use rust_decimal::Decimal;
use std::str::FromStr;

pub(crate) struct Convert;

impl Convert {
    fn parse(s: &str) -> Option<Decimal> {
        let s = s.trim();
        Decimal::from_str_exact(s)
            .or_else(|_| Decimal::from_scientific(s))
            .ok()
    }

    fn truthy(s: &str) -> bool {
        match s.trim() {
            "true" => true,
            "false" => false,
            _ => s.is_empty(),
        }
    }

    pub(crate) const TYPE: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: Arg| Ok(a.type_name()))],
        fail: |_| "Argument on 0 position out of bounds".to_string(),
    };

    pub(crate) const BOOL: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Arg| {
                Ok(match a {
                    Arg::Null => false,
                    Arg::Bool(b) => b,
                    Arg::Num(n) => !n.is_zero(),
                    Arg::Str(s) => Self::truthy(s),
                    Arg::Var(v) if Date::sourced(v) => Self::truthy(Date::source(v).unwrap_or_default()),
                    Arg::Date(_) | Arg::Var(_) | Arg::List(_) => true,
                })
            })
        }],
        fail: |_| "Argument on 0 position out of bounds".to_string(),
    };

    pub(crate) const STRING: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Arg| match a {
                Arg::Null => Ok("null".to_string()),
                Arg::Bool(b) => Ok(b.to_string()),
                Arg::Num(n) => Ok(n.to_string()),
                Arg::Str(s) => Ok(s.to_string()),
                Arg::Date(d) => Ok(d.to_string()),
                Arg::Var(v @ Variable::Dynamic(d)) if Date::of(v).is_some() => Ok(d.to_string()),
                other => Err(format!(
                    "Cannot convert type {} to string",
                    other.type_name()
                )),
            })
        }],
        fail: |_| "Argument on 0 position out of bounds".to_string(),
    };

    pub(crate) const NUMBER: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Arg| match a {
                Arg::Num(n) => Ok(n),
                Arg::Str(s) => Self::parse(s).ok_or_else(|| "Invalid number".to_string()),
                Arg::Bool(true) => Ok(Decimal::ONE),
                Arg::Bool(false) => Ok(Decimal::ZERO),
                other => Err(format!(
                    "Cannot convert type {} to number",
                    other.type_name()
                )),
            })
        }],
        fail: |_| "Argument on 0 position out of bounds".to_string(),
    };

    pub(crate) const IS_NUMERIC: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Arg| {
                Ok(match a {
                    Arg::Num(_) => true,
                    Arg::Str(s) => Self::parse(s).is_some(),
                    _ => false,
                })
            })
        }],
        fail: |_| "Argument on 0 position out of bounds".to_string(),
    };

    pub(crate) const DATE: Builtin = Builtin {
        overloads: &[|args| {
            Apply::two(args, |value: Option<Arg>, zone: Option<&str>| {
                let zone = match zone {
                    Some(z) => Some(Tz::from_str(z).map_err(|_| "Invalid timezone".to_string())?),
                    None => None,
                };
                Ok(match value {
                    Some(Arg::Date(d)) => zone.map_or(d, |zone| d.tz(zone)),
                    Some(Arg::Str(s)) => Date::text(s, zone),
                    Some(v) => Date::new(v.variable(), zone),
                    None => Date::now(),
                })
            })
        }],
        fail: |args| Params::fail(args, &[("value", true), ("string", true)]),
    };
}

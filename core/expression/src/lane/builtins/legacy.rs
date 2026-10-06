use crate::lane::builtins::{Apply, Arg, Builtin, Fail, Params};
use crate::lane::date::helpers::{date_time, date_time_end_of, date_time_start_of, time, DateUnit};
use crate::variable::Variable;
use chrono::{Datelike, NaiveDateTime, Timelike};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

pub(crate) struct Legacy;

impl Legacy {
    const ANY: fn(&[Arg]) -> Fail = |_| "Argument on 0 position out of bounds".to_string();

    fn convert(timestamp: Arg) -> Result<NaiveDateTime, Fail> {
        let instant = timestamp
            .date()
            .and_then(|date| date.0)
            .map(|date| date.naive_local());
        let converted = match timestamp {
            Arg::Str(s) => date_time(s).ok(),
            Arg::Date(_) | Arg::Var(Variable::Dynamic(_)) => match timestamp.text() {
                Some(text) => date_time(&text).ok().or(instant),
                None => instant,
            },
            #[allow(deprecated)]
            Arg::Num(n) => n
                .to_i64()
                .and_then(|n| NaiveDateTime::from_timestamp_opt(n, 0)),
            _ => None,
        };
        converted.ok_or_else(|| "Failed to convert value to date time".to_string())
    }

    pub(crate) const DATE: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Arg| match a {
                Arg::Str(s) => {
                    let dt = date_time(s).map_err(|e| e.to_string())?;
                    #[allow(deprecated)]
                    Ok(Decimal::from(dt.timestamp()))
                }
                Arg::Num(n) => n
                    .to_i64()
                    .map(Decimal::from)
                    .ok_or_else(|| "Number overflow".to_string()),
                other => Self::convert(other)
                    .map_err(|_| "Unsupported type for date function".to_string())
                    .map(|dt| {
                        #[allow(deprecated)]
                        Decimal::from(dt.timestamp())
                    }),
            })
        }],
        fail: Self::ANY,
    };

    pub(crate) const TIME: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Arg| match a {
                Arg::Str(s) => Ok(Decimal::from(
                    time(s)
                        .map_err(|e| e.to_string())?
                        .num_seconds_from_midnight(),
                )),
                Arg::Num(n) => n
                    .to_u32()
                    .map(Decimal::from)
                    .ok_or_else(|| "Number overflow".to_string()),
                Arg::Date(_) | Arg::Var(Variable::Dynamic(_)) => {
                    match a.text().map(|text| time(&text)) {
                        Some(Ok(time)) => Ok(Decimal::from(time.num_seconds_from_midnight())),
                        _ => Self::convert(a).map(|dt| Decimal::from(dt.time().num_seconds_from_midnight())),
                    }
                }
                _ => Err("Unsupported type for time function".to_string()),
            })
        }],
        fail: Self::ANY,
    };

    pub(crate) const DURATION: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Arg| match a {
                Arg::Str(s) => Ok(Decimal::from(
                    humantime::parse_duration(s)
                        .map_err(|e| e.to_string())?
                        .as_secs(),
                )),
                Arg::Num(n) => n
                    .to_u64()
                    .map(Decimal::from)
                    .ok_or_else(|| "Number overflow".to_string()),
                _ => Err("Unsupported type for duration function".to_string()),
            })
        }],
        fail: Self::ANY,
    };

    pub(crate) const YEAR: Builtin = Builtin {
        overloads: &[|args| {
            Some(Self::convert(*args.first()?).map(|t| Decimal::from(t.year()).into()))
        }],
        fail: Self::ANY,
    };

    pub(crate) const DAY_OF_WEEK: Builtin = Builtin {
        overloads: &[|args| {
            Some(
                Self::convert(*args.first()?)
                    .map(|t| Decimal::from(t.weekday().number_from_monday()).into()),
            )
        }],
        fail: Self::ANY,
    };

    pub(crate) const DAY_OF_MONTH: Builtin = Builtin {
        overloads: &[|args| {
            Some(Self::convert(*args.first()?).map(|t| Decimal::from(t.day()).into()))
        }],
        fail: Self::ANY,
    };

    pub(crate) const DAY_OF_YEAR: Builtin = Builtin {
        overloads: &[|args| {
            Some(Self::convert(*args.first()?).map(|t| Decimal::from(t.ordinal()).into()))
        }],
        fail: Self::ANY,
    };

    pub(crate) const WEEK_OF_YEAR: Builtin = Builtin {
        overloads: &[|args| {
            Some(Self::convert(*args.first()?).map(|t| Decimal::from(t.iso_week().week()).into()))
        }],
        fail: Self::ANY,
    };

    pub(crate) const MONTH_OF_YEAR: Builtin = Builtin {
        overloads: &[|args| {
            Some(Self::convert(*args.first()?).map(|t| Decimal::from(t.month()).into()))
        }],
        fail: Self::ANY,
    };

    pub(crate) const MONTH_STRING: Builtin = Builtin {
        overloads: &[|args| {
            Some(Self::convert(*args.first()?).map(|t| t.format("%b").to_string().into()))
        }],
        fail: Self::ANY,
    };

    pub(crate) const WEEKDAY_STRING: Builtin = Builtin {
        overloads: &[|args| {
            Some(Self::convert(*args.first()?).map(|t| t.weekday().to_string().into()))
        }],
        fail: Self::ANY,
    };

    pub(crate) const DATE_STRING: Builtin = Builtin {
        overloads: &[|args| Some(Self::convert(*args.first()?).map(|t| t.to_string().into()))],
        fail: Self::ANY,
    };

    pub(crate) const START_OF: Builtin = Builtin {
        overloads: &[|args| {
            Apply::two(args, |timestamp: Arg, unit: &str| {
                let datetime = Self::convert(timestamp)?;
                let unit = DateUnit::try_from(unit).map_err(|_| "Invalid date unit".to_string())?;
                let result = date_time_start_of(datetime, unit)
                    .ok_or_else(|| "Failed to calculate start of period".to_string())?;
                #[allow(deprecated)]
                Ok(Decimal::from(result.timestamp()))
            })
        }],
        fail: |args| Params::fail(args, &[("value", false), ("string", false)]),
    };

    pub(crate) const END_OF: Builtin = Builtin {
        overloads: &[|args| {
            Apply::two(args, |timestamp: Arg, unit: &str| {
                let datetime = Self::convert(timestamp)?;
                let unit = DateUnit::try_from(unit).map_err(|_| "Invalid date unit".to_string())?;
                let result = date_time_end_of(datetime, unit)
                    .ok_or_else(|| "Failed to calculate end of period".to_string())?;
                #[allow(deprecated)]
                Ok(Decimal::from(result.timestamp()))
            })
        }],
        fail: |args| Params::fail(args, &[("value", false), ("string", false)]),
    };
}

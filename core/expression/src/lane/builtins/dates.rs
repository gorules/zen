use crate::functions::{DateMethod, MethodKind};
use crate::lane::builtins::{Arg, Builtin, Fail, Out, Outcome};
use crate::lane::date::Date;
use crate::lane::date::{Duration, DurationUnit};
use crate::variable::Variable;
use chrono::{Datelike, Timelike};
use chrono_tz::Tz;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use std::borrow::Cow;
use std::str::FromStr;

pub(crate) struct Dates;

pub(crate) enum Shift {
    Add(Duration),
    Sub(Duration),
    Start(DurationUnit),
    End(DurationUnit),
}

impl Shift {
    #[inline]
    pub(crate) fn apply(&self, date: &Date) -> Date {
        match self {
            Shift::Add(d) => date.add(d.clone()),
            Shift::Sub(d) => date.sub(d.clone()),
            Shift::Start(unit) => date.start_of(*unit),
            Shift::End(unit) => date.end_of(*unit),
        }
    }
}

#[derive(Clone, Copy)]
enum Compare {
    Same,
    Before,
    After,
    SameOrBefore,
    SameOrAfter,
}

#[derive(Clone, Copy)]
enum Part {
    Second,
    Minute,
    Hour,
    Day,
    Weekday,
    DayOfYear,
    Week,
    Month,
    Quarter,
    Year,
    Timestamp,
    OffsetName,
    IsValid,
    IsYesterday,
    IsToday,
    IsTomorrow,
    IsLeapYear,
}

impl Dates {
    const NEVER: fn(&[Arg]) -> Fail =
        |_| "Argument on 0 position is not a valid dynamic".to_string();

    fn this(args: &[Arg]) -> Result<Date, Fail> {
        args.first()
            .and_then(Arg::date)
            .ok_or_else(|| "Argument on 0 position is not a valid dynamic".to_string())
    }

    fn var<'a>(args: &[Arg<'a>], pos: usize) -> Result<Arg<'a>, Fail> {
        args.get(pos)
            .copied()
            .ok_or_else(|| format!("Argument on {pos} position out of bounds"))
    }

    fn ostr<'a>(args: &[Arg<'a>], pos: usize) -> Result<Option<Cow<'a, str>>, Fail> {
        match args.get(pos) {
            None => Ok(None),
            Some(a) => a
                .text()
                .map(Some)
                .ok_or_else(|| format!("Argument on {pos} is not a string")),
        }
    }

    fn str<'a>(args: &[Arg<'a>], pos: usize) -> Result<Cow<'a, str>, Fail> {
        Self::ostr(args, pos)?
            .ok_or_else(|| format!("Argument on {pos} position is not a valid string"))
    }

    fn number(args: &[Arg], pos: usize) -> Result<Decimal, Fail> {
        match args.get(pos) {
            None => Err(format!("Argument on {pos} position is not a valid number")),
            Some(Arg::Num(n)) => Ok(*n),
            Some(_) => Err(format!("Argument on {pos} is not a number")),
        }
    }

    fn unit(args: &[Arg], pos: usize) -> Result<DurationUnit, Fail> {
        DurationUnit::parse(&Self::str(args, pos)?)
            .ok_or_else(|| "Invalid duration unit".to_string())
    }

    fn unit_opt(args: &[Arg], pos: usize) -> Result<Option<DurationUnit>, Fail> {
        match Self::ostr(args, pos)? {
            None => Ok(None),
            Some(u) => DurationUnit::parse(&u)
                .map(Some)
                .ok_or_else(|| "Invalid duration unit".to_string()),
        }
    }

    fn duration(args: &[Arg], from: usize) -> Result<Duration, Fail> {
        match Self::var(args, from)? {
            Arg::Str(s) => Duration::parse(s).map_err(|e| e.to_string()),
            Arg::Num(n) => {
                let unit = Self::unit(args, from + 1)?;
                Duration::from_unit(n, unit).ok_or_else(|| "Invalid duration unit".to_string())
            }
            _ => Err("Invalid duration arguments".to_string()),
        }
    }

    fn other(args: &[Arg]) -> Result<Date, Fail> {
        Ok(match Self::var(args, 1)? {
            Arg::Date(d) => d,
            Arg::Str(s) => Date::text(s, None),
            other => Date::new(other.variable(), None),
        })
    }

    fn date(d: Date) -> Out {
        Out::Date(d)
    }

    fn run(f: impl FnOnce() -> Result<Out, Fail>) -> Outcome {
        Some(f())
    }

    pub(crate) const ADD: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| Ok(Self::date(Self::this(args)?.add(Self::duration(args, 1)?))))
        }],
        fail: Self::NEVER,
    };

    pub(crate) const SUB: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| Ok(Self::date(Self::this(args)?.sub(Self::duration(args, 1)?))))
        }],
        fail: Self::NEVER,
    };

    pub(crate) const SET: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| {
                let this = Self::this(args)?;
                let unit = Self::unit(args, 1)?;
                let value = Self::number(args, 2)?
                    .to_u32()
                    .ok_or_else(|| "Invalid duration value".to_string())?;
                Ok(Self::date(this.set(value, unit)))
            })
        }],
        fail: Self::NEVER,
    };

    pub(crate) const FORMAT: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| {
                let this = Self::this(args)?;
                Ok(this.format(Self::ostr(args, 1)?.as_deref()).into())
            })
        }],
        fail: Self::NEVER,
    };

    pub(crate) const START_OF: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| Ok(Self::date(Self::this(args)?.start_of(Self::unit(args, 1)?))))
        }],
        fail: Self::NEVER,
    };

    pub(crate) const END_OF: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| Ok(Self::date(Self::this(args)?.end_of(Self::unit(args, 1)?))))
        }],
        fail: Self::NEVER,
    };

    pub(crate) const DIFF: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| {
                let this = Self::this(args)?;
                let other = Self::other(args)?;
                let unit = Self::unit_opt(args, 2)?;
                Ok(match this.diff(&other, unit).and_then(Decimal::from_i64) {
                    Some(n) => Out::Num(n),
                    None => Out::Var(Variable::Null),
                })
            })
        }],
        fail: Self::NEVER,
    };

    pub(crate) const TZ: Builtin = Builtin {
        overloads: &[|args| {
            Self::run(|| {
                let this = Self::this(args)?;
                let zone = Tz::from_str(&Self::str(args, 1)?)
                    .map_err(|_| "Invalid timezone".to_string())?;
                Ok(Self::date(this.tz(zone)))
            })
        }],
        fail: Self::NEVER,
    };

    fn compare(args: &[Arg], op: Compare) -> Outcome {
        Self::run(|| {
            let this = Self::this(args)?;
            let other = Self::other(args)?;
            let unit = Self::unit_opt(args, 2)?;
            Ok(Out::Bool(match op {
                Compare::Same => this.is_same(&other, unit),
                Compare::Before => this.is_before(&other, unit),
                Compare::After => this.is_after(&other, unit),
                Compare::SameOrBefore => this.is_same_or_before(&other, unit),
                Compare::SameOrAfter => this.is_same_or_after(&other, unit),
            }))
        })
    }

    pub(crate) const IS_SAME: Builtin = Builtin {
        overloads: &[|args| Self::compare(args, Compare::Same)],
        fail: Self::NEVER,
    };

    pub(crate) const IS_BEFORE: Builtin = Builtin {
        overloads: &[|args| Self::compare(args, Compare::Before)],
        fail: Self::NEVER,
    };

    pub(crate) const IS_AFTER: Builtin = Builtin {
        overloads: &[|args| Self::compare(args, Compare::After)],
        fail: Self::NEVER,
    };

    pub(crate) const IS_SAME_OR_BEFORE: Builtin = Builtin {
        overloads: &[|args| Self::compare(args, Compare::SameOrBefore)],
        fail: Self::NEVER,
    };

    pub(crate) const IS_SAME_OR_AFTER: Builtin = Builtin {
        overloads: &[|args| Self::compare(args, Compare::SameOrAfter)],
        fail: Self::NEVER,
    };

    pub(crate) fn shift(method: &MethodKind, args: &[Arg]) -> Option<Shift> {
        let MethodKind::DateMethod(method) = method;
        match method {
            DateMethod::Add => Self::duration(args, 1).ok().map(Shift::Add),
            DateMethod::Sub => Self::duration(args, 1).ok().map(Shift::Sub),
            DateMethod::StartOf if args.len() == 2 => Self::unit(args, 1).ok().map(Shift::Start),
            DateMethod::EndOf if args.len() == 2 => Self::unit(args, 1).ok().map(Shift::End),
            _ => None,
        }
    }

    pub(crate) fn part_of(method: &MethodKind, date: &Date) -> Option<i64> {
        let MethodKind::DateMethod(method) = method;
        let dt = date.0?;
        Some(match method {
            DateMethod::Second => dt.second() as i64,
            DateMethod::Minute => dt.minute() as i64,
            DateMethod::Hour => dt.hour() as i64,
            DateMethod::Day => dt.day() as i64,
            DateMethod::Weekday => dt.weekday().number_from_monday() as i64,
            DateMethod::DayOfYear => dt.ordinal() as i64,
            DateMethod::Week => dt.iso_week().week() as i64,
            DateMethod::Month => dt.month() as i64,
            DateMethod::Quarter => dt.quarter() as i64,
            DateMethod::Year => dt.year() as i64,
            DateMethod::Timestamp => dt.timestamp_millis(),
            _ => return None,
        })
    }

    pub(crate) fn order(method: &MethodKind, this: &Date, other: &Date) -> Option<bool> {
        let MethodKind::DateMethod(method) = method;
        Some(match method {
            DateMethod::IsSame => this.is_same(other, None),
            DateMethod::IsBefore => this.is_before(other, None),
            DateMethod::IsAfter => this.is_after(other, None),
            DateMethod::IsSameOrBefore => this.is_same_or_before(other, None),
            DateMethod::IsSameOrAfter => this.is_same_or_after(other, None),
            _ => return None,
        })
    }

    fn part(args: &[Arg], part: Part) -> Outcome {
        Self::run(|| {
            let this = Self::this(args)?;
            if let Part::IsValid = part {
                return Ok(Out::Bool(this.is_valid()));
            }
            let Some(dt) = this.0 else {
                return Ok(Out::Var(Variable::Null));
            };
            Ok(match part {
                Part::Second => Out::Num(dt.second().into()),
                Part::Minute => Out::Num(dt.minute().into()),
                Part::Hour => Out::Num(dt.hour().into()),
                Part::Day => Out::Num(dt.day().into()),
                Part::Weekday => Out::Num(dt.weekday().number_from_monday().into()),
                Part::DayOfYear => Out::Num(dt.ordinal().into()),
                Part::Week => Out::Num(dt.iso_week().week().into()),
                Part::Month => Out::Num(dt.month().into()),
                Part::Quarter => Out::Num(dt.quarter().into()),
                Part::Year => Out::Num(dt.year().into()),
                Part::Timestamp => Out::Num(dt.timestamp_millis().into()),
                Part::IsValid => Out::Bool(true),
                Part::IsYesterday => {
                    Out::Bool(this.is_same(&Date::yesterday(), Some(DurationUnit::Day)))
                }
                Part::IsToday => Out::Bool(this.is_same(&Date::now(), Some(DurationUnit::Day))),
                Part::IsTomorrow => {
                    Out::Bool(this.is_same(&Date::tomorrow(), Some(DurationUnit::Day)))
                }
                Part::IsLeapYear => Out::Bool(dt.date_naive().leap_year()),
                Part::OffsetName => dt.timezone().name().into(),
            })
        })
    }

    pub(crate) const SECOND: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Second)],
        fail: Self::NEVER,
    };
    pub(crate) const MINUTE: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Minute)],
        fail: Self::NEVER,
    };
    pub(crate) const HOUR: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Hour)],
        fail: Self::NEVER,
    };
    pub(crate) const DAY: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Day)],
        fail: Self::NEVER,
    };
    pub(crate) const WEEKDAY: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Weekday)],
        fail: Self::NEVER,
    };
    pub(crate) const DAY_OF_YEAR: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::DayOfYear)],
        fail: Self::NEVER,
    };
    pub(crate) const WEEK: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Week)],
        fail: Self::NEVER,
    };
    pub(crate) const MONTH: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Month)],
        fail: Self::NEVER,
    };
    pub(crate) const QUARTER: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Quarter)],
        fail: Self::NEVER,
    };
    pub(crate) const YEAR: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Year)],
        fail: Self::NEVER,
    };
    pub(crate) const TIMESTAMP: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::Timestamp)],
        fail: Self::NEVER,
    };
    pub(crate) const OFFSET_NAME: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::OffsetName)],
        fail: Self::NEVER,
    };
    pub(crate) const IS_VALID: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::IsValid)],
        fail: Self::NEVER,
    };
    pub(crate) const IS_YESTERDAY: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::IsYesterday)],
        fail: Self::NEVER,
    };
    pub(crate) const IS_TODAY: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::IsToday)],
        fail: Self::NEVER,
    };
    pub(crate) const IS_TOMORROW: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::IsTomorrow)],
        fail: Self::NEVER,
    };
    pub(crate) const IS_LEAP_YEAR: Builtin = Builtin {
        overloads: &[|a| Self::part(a, Part::IsLeapYear)],
        fail: Self::NEVER,
    };
}

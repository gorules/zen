use crate::functions::arguments::Arguments;
use crate::functions::defs::{FunctionDefinition, FunctionSignature, StaticFunction};
use crate::vm::helpers::{date_time, date_time_end_of, date_time_start_of, time};
use crate::Variable as V;
use anyhow::{anyhow, Context};
use chrono::{Datelike, NaiveDateTime, Timelike};
use rust_decimal::prelude::ToPrimitive;
use std::rc::Rc;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum DeprecatedFunction {
    Date,
    Time,
    Duration,
    Year,
    DayOfWeek,
    DayOfMonth,
    DayOfYear,
    WeekOfYear,
    MonthOfYear,
    MonthString,
    DateString,
    WeekdayString,
    StartOf,
    EndOf,
}

impl From<&DeprecatedFunction> for Rc<dyn FunctionDefinition> {
    fn from(value: &DeprecatedFunction) -> Self {
        use crate::variable::VariableType as VT;
        use DeprecatedFunction as DF;

        let s: Rc<dyn FunctionDefinition> = match value {
            DF::Date => Rc::new(StaticFunction {
                implementation: Rc::new(imp::parse_date),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::Time => Rc::new(StaticFunction {
                implementation: Rc::new(imp::parse_time),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::Duration => Rc::new(StaticFunction {
                implementation: Rc::new(imp::parse_duration),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::Year => Rc::new(StaticFunction {
                implementation: Rc::new(imp::year),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::DayOfWeek => Rc::new(StaticFunction {
                implementation: Rc::new(imp::day_of_week),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::DayOfMonth => Rc::new(StaticFunction {
                implementation: Rc::new(imp::day_of_month),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::DayOfYear => Rc::new(StaticFunction {
                implementation: Rc::new(imp::day_of_year),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::WeekOfYear => Rc::new(StaticFunction {
                implementation: Rc::new(imp::week_of_year),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::MonthOfYear => Rc::new(StaticFunction {
                implementation: Rc::new(imp::month_of_year),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            DF::MonthString => Rc::new(StaticFunction {
                implementation: Rc::new(imp::month_string),
                signature: FunctionSignature::single(VT::Any, VT::String),
            }),

            DF::DateString => Rc::new(StaticFunction {
                implementation: Rc::new(imp::date_string),
                signature: FunctionSignature::single(VT::Any, VT::String),
            }),

            DF::WeekdayString => Rc::new(StaticFunction {
                implementation: Rc::new(imp::weekday_string),
                signature: FunctionSignature::single(VT::Any, VT::String),
            }),

            DF::StartOf => Rc::new(StaticFunction {
                implementation: Rc::new(imp::start_of),
                signature: FunctionSignature {
                    parameters: vec![VT::Any, VT::String],
                    return_type: VT::Number,
                },
            }),

            DF::EndOf => Rc::new(StaticFunction {
                implementation: Rc::new(imp::end_of),
                signature: FunctionSignature {
                    parameters: vec![VT::Any, VT::String],
                    return_type: VT::Number,
                },
            }),
        };

        s
    }
}

mod imp {
    use super::*;
    use crate::vm::helpers::DateUnit;

    fn __internal_convert_datetime(timestamp: &V) -> anyhow::Result<NaiveDateTime> {
        timestamp
            .try_into()
            .context("Failed to convert value to date time")
    }

    pub fn parse_date(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;

        let ts = match a {
            V::String(a) => {
                let dt = date_time(a.as_ref())?;
                #[allow(deprecated)]
                dt.timestamp()
            }
            V::Number(a) => a.to_i64().context("Number overflow")?,
            _ => return Err(anyhow!("Unsupported type for date function")),
        };

        Ok(V::Number(ts.into()))
    }

    pub fn parse_time(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;

        let ts = match a {
            V::String(a) => time(a.as_ref())?.num_seconds_from_midnight(),
            V::Number(a) => a.to_u32().context("Number overflow")?,
            _ => return Err(anyhow!("Unsupported type for time function")),
        };

        Ok(V::Number(ts.into()))
    }

    pub fn parse_duration(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;

        let dur = match a {
            V::String(a) => humantime::parse_duration(a.as_ref())?.as_secs(),
            V::Number(n) => n.to_u64().context("Number overflow")?,
            _ => return Err(anyhow!("Unsupported type for duration function")),
        };

        Ok(V::Number(dur.into()))
    }

    pub fn year(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::Number(time.year().into()))
    }

    pub fn day_of_week(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::Number(time.weekday().number_from_monday().into()))
    }

    pub fn day_of_month(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::Number(time.day().into()))
    }

    pub fn day_of_year(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::Number(time.ordinal().into()))
    }

    pub fn week_of_year(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::Number(time.iso_week().week().into()))
    }

    pub fn month_of_year(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::Number(time.month().into()))
    }

    pub fn month_string(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::String(Rc::from(time.format("%b").to_string())))
    }

    pub fn weekday_string(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::String(Rc::from(time.weekday().to_string())))
    }

    pub fn date_string(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let time = __internal_convert_datetime(&timestamp)?;
        Ok(V::String(Rc::from(time.to_string())))
    }

    pub fn start_of(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let unit_name = args.str(1)?;

        let datetime = __internal_convert_datetime(&timestamp)?;
        let unit = DateUnit::try_from(unit_name).context("Invalid date unit")?;

        let result =
            date_time_start_of(datetime, unit).context("Failed to calculate start of period")?;

        #[allow(deprecated)]
        Ok(V::Number(result.timestamp().into()))
    }

    pub fn end_of(args: Arguments) -> anyhow::Result<V> {
        let timestamp = args.var(0)?;
        let unit_name = args.str(1)?;

        let datetime = __internal_convert_datetime(&timestamp)?;
        let unit = DateUnit::try_from(unit_name).context("Invalid date unit")?;

        let result =
            date_time_end_of(datetime, unit).context("Failed to calculate end of period")?;

        #[allow(deprecated)]
        Ok(V::Number(result.timestamp().into()))
    }
}

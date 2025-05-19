use crate::functions::defs::{
    CompositeFunction, FunctionDefinition, FunctionSignature, StaticFunction,
};
use crate::vm::date::DurationUnit;
use std::rc::Rc;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum DateMethod {
    Add,
    Sub,
    Set,
    Format,
    StartOf,
    EndOf,
    Diff,
    Tz,

    // Compare
    IsSame,
    IsBefore,
    IsAfter,
    IsSameOrBefore,
    IsSameOrAfter,

    // Getters
    Second,
    Minute,
    Hour,
    Day,
    DayOfYear,
    Week,
    Weekday,
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

enum CompareOperation {
    IsSame,
    IsBefore,
    IsAfter,
    IsSameOrBefore,
    IsSameOrAfter,
}

enum GetterOperation {
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

impl From<&DateMethod> for Rc<dyn FunctionDefinition> {
    fn from(value: &DateMethod) -> Self {
        use crate::variable::VariableType as VT;
        use DateMethod as DM;

        let unit_vt = DurationUnit::variable_type();

        let op_signature = vec![
            FunctionSignature {
                parameters: vec![VT::Date, VT::String],
                return_type: VT::Date,
            },
            FunctionSignature {
                parameters: vec![VT::Date, VT::Number, unit_vt.clone()],
                return_type: VT::Date,
            },
        ];

        match value {
            DM::Add => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::add),
                signatures: op_signature.clone(),
            }),
            DM::Sub => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::sub),
                signatures: op_signature.clone(),
            }),
            DM::Set => Rc::new(StaticFunction {
                implementation: Rc::new(imp::set),
                signature: FunctionSignature {
                    parameters: vec![VT::Date, unit_vt.clone(), VT::Number],
                    return_type: VT::Date,
                },
            }),
            DM::Tz => Rc::new(StaticFunction {
                implementation: Rc::new(imp::tz),
                signature: FunctionSignature {
                    parameters: vec![VT::Date, VT::String],
                    return_type: VT::Date,
                },
            }),
            DM::Format => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::format),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::Date],
                        return_type: VT::String,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::String],
                        return_type: VT::String,
                    },
                ],
            }),
            DM::StartOf => Rc::new(StaticFunction {
                implementation: Rc::new(imp::start_of),
                signature: FunctionSignature {
                    parameters: vec![VT::Date, unit_vt.clone()],
                    return_type: VT::Date,
                },
            }),
            DM::EndOf => Rc::new(StaticFunction {
                implementation: Rc::new(imp::end_of),
                signature: FunctionSignature {
                    parameters: vec![VT::Date, unit_vt.clone()],
                    return_type: VT::Date,
                },
            }),
            DM::Diff => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::diff),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::Date],
                        return_type: VT::Number,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::Date, unit_vt.clone()],
                        return_type: VT::Number,
                    },
                ],
            }),
            DateMethod::IsSame => imp::compare_using(CompareOperation::IsSame),
            DateMethod::IsBefore => imp::compare_using(CompareOperation::IsBefore),
            DateMethod::IsAfter => imp::compare_using(CompareOperation::IsAfter),
            DateMethod::IsSameOrBefore => imp::compare_using(CompareOperation::IsSameOrBefore),
            DateMethod::IsSameOrAfter => imp::compare_using(CompareOperation::IsSameOrAfter),

            DateMethod::Second => imp::getter(GetterOperation::Second),
            DateMethod::Minute => imp::getter(GetterOperation::Minute),
            DateMethod::Hour => imp::getter(GetterOperation::Hour),
            DateMethod::Day => imp::getter(GetterOperation::Day),
            DateMethod::Weekday => imp::getter(GetterOperation::Weekday),
            DateMethod::DayOfYear => imp::getter(GetterOperation::DayOfYear),
            DateMethod::Week => imp::getter(GetterOperation::Week),
            DateMethod::Month => imp::getter(GetterOperation::Month),
            DateMethod::Quarter => imp::getter(GetterOperation::Quarter),
            DateMethod::Year => imp::getter(GetterOperation::Year),
            DateMethod::Timestamp => imp::getter(GetterOperation::Timestamp),
            DateMethod::OffsetName => imp::getter(GetterOperation::OffsetName),

            DateMethod::IsValid => imp::getter(GetterOperation::IsValid),
            DateMethod::IsYesterday => imp::getter(GetterOperation::IsYesterday),
            DateMethod::IsToday => imp::getter(GetterOperation::IsToday),
            DateMethod::IsTomorrow => imp::getter(GetterOperation::IsTomorrow),
            DateMethod::IsLeapYear => imp::getter(GetterOperation::IsLeapYear),
        }
    }
}

mod imp {
    use crate::functions::arguments::Arguments;
    use crate::functions::date_method::{CompareOperation, GetterOperation};
    use crate::functions::defs::{
        CompositeFunction, FunctionDefinition, FunctionSignature, StaticFunction,
    };
    use crate::variable::VariableType as VT;
    use crate::vm::date::{Duration, DurationUnit};
    use crate::vm::VmDate;
    use crate::Variable as V;
    use anyhow::{anyhow, Context};
    use chrono::{Datelike, Timelike};
    use chrono_tz::Tz;
    use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
    use rust_decimal::Decimal;
    use std::rc::Rc;
    use std::str::FromStr;

    fn __internal_extract_duration(args: &Arguments, from: usize) -> anyhow::Result<Duration> {
        match args.var(from)? {
            V::String(s) => Ok(Duration::parse(s.as_ref())?),
            V::Number(n) => {
                let unit = __internal_extract_duration_unit(args, from + 1)?;
                Ok(Duration::from_unit(*n, unit).context("Invalid duration unit")?)
            }
            _ => Err(anyhow!("Invalid duration arguments")),
        }
    }

    fn __internal_extract_duration_unit(
        args: &Arguments,
        pos: usize,
    ) -> anyhow::Result<DurationUnit> {
        let unit_str = args.str(pos)?;
        DurationUnit::parse(unit_str).context("Invalid duration unit")
    }

    fn __internal_extract_duration_unit_opt(
        args: &Arguments,
        pos: usize,
    ) -> anyhow::Result<Option<DurationUnit>> {
        let unit_ostr = args.ostr(pos)?;
        let Some(unit_str) = unit_ostr else {
            return Ok(None);
        };

        Ok(Some(
            DurationUnit::parse(unit_str).context("Invalid duration unit")?,
        ))
    }

    pub fn add(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let duration = __internal_extract_duration(&args, 1)?;

        let date_time = this.add(duration);
        Ok(V::Dynamic(Rc::new(date_time)))
    }

    pub fn sub(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let duration = __internal_extract_duration(&args, 1)?;

        let date_time = this.sub(duration);
        Ok(V::Dynamic(Rc::new(date_time)))
    }

    pub fn set(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let unit = __internal_extract_duration_unit(&args, 1)?;
        let value = args.number(2)?;

        let value_u32 = value.to_u32().context("Invalid duration value")?;

        let date_time = this.set(value_u32, unit);
        Ok(V::Dynamic(Rc::new(date_time)))
    }

    pub fn format(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let format = args.ostr(1)?;

        let formatted = this.format(format);
        Ok(V::String(Rc::from(formatted)))
    }

    pub fn start_of(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let unit = __internal_extract_duration_unit(&args, 1)?;

        let date_time = this.start_of(unit);
        Ok(V::Dynamic(Rc::new(date_time)))
    }

    pub fn end_of(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let unit = __internal_extract_duration_unit(&args, 1)?;

        let date_time = this.end_of(unit);
        Ok(V::Dynamic(Rc::new(date_time)))
    }

    pub fn diff(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let date_time = VmDate::new(args.var(1)?.clone(), None);
        let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

        let var = match this
            .diff(&date_time, maybe_unit)
            .and_then(Decimal::from_i64)
        {
            Some(n) => V::Number(n),
            None => V::Null,
        };

        Ok(var)
    }

    pub fn tz(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let tz_str = args.str(1)?;

        let timezone = Tz::from_str(tz_str).context("Invalid timezone")?;
        Ok(V::Dynamic(Rc::new(this.tz(timezone))))
    }

    pub fn compare_using(op: CompareOperation) -> Rc<dyn FunctionDefinition> {
        Rc::new(CompositeFunction {
            signatures: vec![
                FunctionSignature {
                    parameters: vec![VT::Date, VT::Date],
                    return_type: VT::Date,
                },
                FunctionSignature {
                    parameters: vec![VT::Date, VT::Date, DurationUnit::variable_type()],
                    return_type: VT::Date,
                },
            ],
            implementation: Rc::new(move |args: Arguments| -> anyhow::Result<V> {
                let this = args.dynamic::<VmDate>(0)?;
                let date_time = VmDate::new(args.var(1)?.clone(), None);
                let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

                let check = match op {
                    CompareOperation::IsSame => this.is_same(&date_time, maybe_unit),
                    CompareOperation::IsBefore => this.is_before(&date_time, maybe_unit),
                    CompareOperation::IsAfter => this.is_after(&date_time, maybe_unit),
                    CompareOperation::IsSameOrBefore => {
                        this.is_same_or_before(&date_time, maybe_unit)
                    }
                    CompareOperation::IsSameOrAfter => {
                        this.is_same_or_after(&date_time, maybe_unit)
                    }
                };

                Ok(V::Bool(check))
            }),
        })
    }

    pub fn getter(op: GetterOperation) -> Rc<dyn FunctionDefinition> {
        Rc::new(StaticFunction {
            signature: FunctionSignature {
                parameters: vec![VT::Date],
                return_type: match op {
                    GetterOperation::Second
                    | GetterOperation::Minute
                    | GetterOperation::Hour
                    | GetterOperation::Day
                    | GetterOperation::Weekday
                    | GetterOperation::DayOfYear
                    | GetterOperation::Week
                    | GetterOperation::Month
                    | GetterOperation::Quarter
                    | GetterOperation::Year
                    | GetterOperation::Timestamp => VT::Number,
                    GetterOperation::IsValid
                    | GetterOperation::IsYesterday
                    | GetterOperation::IsToday
                    | GetterOperation::IsTomorrow
                    | GetterOperation::IsLeapYear => VT::Bool,
                    GetterOperation::OffsetName => VT::String,
                },
            },
            implementation: Rc::new(move |args: Arguments| -> anyhow::Result<V> {
                let this = args.dynamic::<VmDate>(0)?;
                if let GetterOperation::IsValid = op {
                    return Ok(V::Bool(this.is_valid()));
                }

                let Some(dt) = this.0 else {
                    return Ok(V::Null);
                };

                Ok(match op {
                    GetterOperation::Second => V::Number(dt.second().into()),
                    GetterOperation::Minute => V::Number(dt.minute().into()),
                    GetterOperation::Hour => V::Number(dt.hour().into()),
                    GetterOperation::Day => V::Number(dt.day().into()),
                    GetterOperation::Weekday => V::Number(dt.weekday().number_from_monday().into()),
                    GetterOperation::DayOfYear => V::Number(dt.ordinal().into()),
                    GetterOperation::Week => V::Number(dt.iso_week().week().into()),
                    GetterOperation::Month => V::Number(dt.month().into()),
                    GetterOperation::Quarter => V::Number(dt.quarter().into()),
                    GetterOperation::Year => V::Number(dt.year().into()),
                    GetterOperation::Timestamp => V::Number(dt.timestamp_millis().into()),
                    // Boolean
                    GetterOperation::IsValid => V::Bool(true),
                    GetterOperation::IsYesterday => {
                        V::Bool(this.is_same(&VmDate::yesterday(), Some(DurationUnit::Day)))
                    }
                    GetterOperation::IsToday => {
                        V::Bool(this.is_same(&VmDate::now(), Some(DurationUnit::Day)))
                    }
                    GetterOperation::IsTomorrow => {
                        V::Bool(this.is_same(&VmDate::tomorrow(), Some(DurationUnit::Day)))
                    }
                    GetterOperation::IsLeapYear => V::Bool(dt.date_naive().leap_year()),
                    // String
                    GetterOperation::OffsetName => V::String(Rc::from(dt.timezone().name())),
                })
            }),
        })
    }
}

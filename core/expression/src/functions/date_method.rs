use crate::functions::defs::{
    CompositeFunction, FunctionDefinition, FunctionSignature, StaticFunction,
};
use std::rc::Rc;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum DateMethod {
    Add,
    Sub,
    Format,
    StartOf,
    EndOf,
    Diff,
    // IsValid - TODO?

    // Compare
    IsSame,
    IsBefore,
    IsAfter,
    IsSameOrBefore,
    IsSameOrAfter,
    // Getters
    // Second,
    // Minute,
    // Hour,
    // Day,
    // Week,
    // Month,
    // Quarter,
    // Year,
}

impl From<&DateMethod> for Rc<dyn FunctionDefinition> {
    fn from(value: &DateMethod) -> Self {
        use crate::variable::VariableType as VT;
        use DateMethod as DM;

        let op_signature = vec![
            FunctionSignature {
                parameters: vec![VT::Date, VT::String],
                return_type: VT::Date,
            },
            FunctionSignature {
                parameters: vec![VT::Date, VT::Number, VT::String],
                return_type: VT::Date,
            },
        ];

        let compare_signature = vec![
            FunctionSignature {
                parameters: vec![VT::Date, VT::Date],
                return_type: VT::Date,
            },
            FunctionSignature {
                parameters: vec![VT::Date, VT::Date, VT::String],
                return_type: VT::Date,
            },
        ];

        match value {
            DM::Add => Rc::new(CompositeFunction {
                implementation: imp::add,
                signatures: op_signature.clone(),
            }),
            DM::Sub => Rc::new(CompositeFunction {
                implementation: imp::sub,
                signatures: op_signature.clone(),
            }),
            DM::Format => Rc::new(CompositeFunction {
                implementation: imp::format,
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::Date],
                        return_type: VT::Date,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::String],
                        return_type: VT::Date,
                    },
                ],
            }),
            DM::StartOf => Rc::new(StaticFunction {
                implementation: imp::start_of,
                signature: FunctionSignature {
                    parameters: vec![VT::Date, VT::String],
                    return_type: VT::Date,
                },
            }),
            DM::EndOf => Rc::new(StaticFunction {
                implementation: imp::end_of,
                signature: FunctionSignature {
                    parameters: vec![VT::Date, VT::String],
                    return_type: VT::Date,
                },
            }),
            DM::Diff => Rc::new(CompositeFunction {
                implementation: imp::diff,
                signatures: compare_signature.clone(),
            }),
            DateMethod::IsSame => Rc::new(CompositeFunction {
                implementation: imp::is_same,
                signatures: compare_signature.clone(),
            }),
            DateMethod::IsBefore => Rc::new(CompositeFunction {
                implementation: imp::is_before,
                signatures: compare_signature.clone(),
            }),
            DateMethod::IsAfter => Rc::new(CompositeFunction {
                implementation: imp::is_after,
                signatures: compare_signature.clone(),
            }),
            DateMethod::IsSameOrBefore => Rc::new(CompositeFunction {
                implementation: imp::is_same_or_before,
                signatures: compare_signature.clone(),
            }),
            DateMethod::IsSameOrAfter => Rc::new(CompositeFunction {
                implementation: imp::is_same_or_after,
                signatures: compare_signature.clone(),
            }),
        }
    }
}

mod imp {
    use crate::functions::arguments::Arguments;
    use crate::vm::date::{Duration, DurationUnit};
    use crate::vm::VmDate;
    use crate::{Variable as V, Variable};
    use anyhow::{anyhow, Context};
    use rust_decimal::prelude::FromPrimitive;
    use rust_decimal::Decimal;
    use std::rc::Rc;

    fn __internal_extract_duration(args: &Arguments, from: usize) -> anyhow::Result<Duration> {
        match args.var(from)? {
            Variable::String(s) => Ok(Duration::parse(s.as_ref())?),
            Variable::Number(n) => {
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
        let date_time = VmDate::new(args.var(1)?.clone());
        let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

        let var = match this
            .diff(&date_time, maybe_unit)
            .and_then(Decimal::from_i64)
        {
            Some(n) => Variable::Number(n),
            None => Variable::Null,
        };

        Ok(var)
    }

    pub fn is_before(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let date_time = VmDate::new(args.var(1)?.clone());
        let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

        Ok(V::Bool(this.is_before(&date_time, maybe_unit)))
    }

    pub fn is_after(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let date_time = VmDate::new(args.var(1)?.clone());
        let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

        Ok(V::Bool(this.is_after(&date_time, maybe_unit)))
    }

    pub fn is_same(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let date_time = VmDate::new(args.var(1)?.clone());
        let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

        Ok(V::Bool(this.is_same(&date_time, maybe_unit)))
    }

    pub fn is_same_or_after(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let date_time = VmDate::new(args.var(1)?.clone());
        let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

        Ok(V::Bool(this.is_same_or_after(&date_time, maybe_unit)))
    }

    pub fn is_same_or_before(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let date_time = VmDate::new(args.var(1)?.clone());
        let maybe_unit = __internal_extract_duration_unit_opt(&args, 2)?;

        Ok(V::Bool(this.is_same_or_before(&date_time, maybe_unit)))
    }
}

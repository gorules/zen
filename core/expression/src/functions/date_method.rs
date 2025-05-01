use crate::functions::defs::{CompositeFunction, FunctionDefinition, FunctionSignature};
use std::rc::Rc;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum DateMethod {
    Add,
    Sub,
    Format,
}

impl From<&DateMethod> for Rc<dyn FunctionDefinition> {
    fn from(value: &DateMethod) -> Self {
        use crate::variable::VariableType as VT;
        use DateMethod as DM;

        match value {
            DM::Add => Rc::new(CompositeFunction {
                implementation: imp::add,
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::String],
                        return_type: VT::Date,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::Number, VT::String],
                        return_type: VT::Date,
                    },
                ],
            }),
            DM::Sub => Rc::new(CompositeFunction {
                implementation: imp::sub,
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::String],
                        return_type: VT::Date,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Date, VT::Number, VT::String],
                        return_type: VT::Date,
                    },
                ],
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
        }
    }
}

mod imp {
    use crate::functions::arguments::Arguments;
    use crate::vm::date::{Duration, DurationUnit};
    use crate::vm::VmDate;
    use crate::{Variable as V, Variable};
    use anyhow::{anyhow, Context};
    use std::rc::Rc;

    fn __internal_extract_duration(args: &Arguments, from: usize) -> anyhow::Result<Duration> {
        match args.var(from)? {
            Variable::String(s) => Ok(Duration::parse(s.as_ref())?),
            Variable::Number(n) => {
                let unit_str = args.str(from + 1)?;
                let unit = DurationUnit::parse(unit_str).context("Invalid duration unit")?;

                Ok(Duration::from_unit(*n, unit).context("Invalid duration unit")?)
            }
            _ => Err(anyhow!("Invalid duration arguments")),
        }
    }

    pub fn add(args: Arguments) -> anyhow::Result<V> {
        let this = args.dynamic::<VmDate>(0)?;
        let duration = __internal_extract_duration(&args, 1)?;

        println!("{:?}", duration);

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
}

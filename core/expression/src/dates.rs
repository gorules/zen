use crate::variable::Variable;
use crate::vm::date::{DurationUnit, VmDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use std::rc::Rc;

pub struct DateValue;

impl DateValue {
    pub fn from_text(text: &str) -> Option<Variable> {
        VmDate::from_text(text).map(|date| Variable::Dynamic(Rc::new(date)))
    }

    pub fn is_text(text: &str) -> bool {
        VmDate::parses(text)
    }

    pub fn source_text(value: &Variable) -> Option<Variable> {
        let text = value.dynamic::<VmDate>()?.source()?;
        Some(Variable::String(text.into()))
    }

    /// `d(a).diff(d(b), unit)` as ZEN evaluates it, for hosts that compile
    /// it: whole units, truncated toward zero (milliseconds without a unit);
    /// null when either is not a date or the unit is unknown.
    pub fn diff(a: &Variable, b: &Variable, unit: Option<&str>) -> Variable {
        let unit = match unit {
            None => None,
            Some(unit) => match DurationUnit::parse(unit) {
                Some(unit) => Some(unit),
                None => return Variable::Null,
            },
        };
        VmDate::new(a.clone(), None)
            .diff(&VmDate::new(b.clone(), None), unit)
            .and_then(Decimal::from_i64)
            .map_or(Variable::Null, Variable::Number)
    }

    pub fn is(value: &Variable) -> bool {
        matches!(value, Variable::Dynamic(dynamic) if dynamic.type_name() == "date")
    }
}

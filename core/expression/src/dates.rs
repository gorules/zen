use crate::variable::Variable;
use crate::vm::date::VmDate;
use std::rc::Rc;

pub struct DateValue;

impl DateValue {
    pub fn from_text(text: &str) -> Option<Variable> {
        VmDate::from_text(text).map(|date| Variable::Dynamic(Rc::new(date)))
    }

    pub fn from_text_in(text: &str, tz: crate::context::Timezone) -> Option<Variable> {
        VmDate::from_text_in(text, tz).map(|date| Variable::Dynamic(Rc::new(date)))
    }

    pub fn is_text(text: &str) -> bool {
        VmDate::parses(text)
    }

    pub fn reads(text: &str) -> bool {
        crate::lane::date::Date::remembered(text, crate::context::system_timezone()).is_valid()
    }

    pub fn source_text(value: &Variable) -> Option<Variable> {
        let text = value.dynamic::<VmDate>()?.source()?;
        Some(Variable::String(text.into()))
    }

    pub fn source(value: &Variable) -> Option<&str> {
        value.dynamic::<VmDate>()?.source()
    }

    pub fn is(value: &Variable) -> bool {
        matches!(value, Variable::Dynamic(dynamic) if dynamic.type_name() == "date")
    }
}

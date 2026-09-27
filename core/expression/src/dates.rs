use crate::variable::Variable;
use crate::vm::date::VmDate;
use std::rc::Rc;

pub fn is_date_text(text: &str) -> bool {
    VmDate::from_text(text).is_some()
}

pub fn date_from_text(text: &str) -> Option<Variable> {
    VmDate::from_text(text).map(|date| Variable::Dynamic(Rc::new(date)))
}

pub fn is_date(value: &Variable) -> bool {
    matches!(value, Variable::Dynamic(dynamic) if dynamic.type_name() == "date")
}

use crate::functions::{
    DateMethod, DeprecatedFunction, FunctionKind, InternalFunction, MethodKind,
};
use crate::lane::date::Date;
use chrono_tz::Tz;
use std::str::FromStr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Effect {
    Pure,
    Clock,
    Random,
}

#[derive(Clone, Copy)]
pub(crate) enum Literal<'a> {
    Text(&'a str),
    Constant,
    Computed,
}

impl Effect {
    pub(crate) fn function(kind: &FunctionKind) -> Self {
        match kind {
            FunctionKind::Internal(InternalFunction::Rand) => Effect::Random,
            FunctionKind::Internal(InternalFunction::Date) => Effect::Clock,
            FunctionKind::Deprecated(DeprecatedFunction::Duration) => Effect::Pure,
            FunctionKind::Deprecated(_) => Effect::Clock,
            FunctionKind::Internal(_) | FunctionKind::Closure(_) => Effect::Pure,
        }
    }

    pub(crate) fn method(kind: &MethodKind) -> Self {
        match kind {
            MethodKind::DateMethod(
                DateMethod::IsToday | DateMethod::IsYesterday | DateMethod::IsTomorrow,
            ) => Effect::Clock,
            MethodKind::DateMethod(_) => Effect::Pure,
        }
    }

    pub(crate) fn call<'a>(
        kind: &FunctionKind,
        mut literals: impl Iterator<Item = Literal<'a>>,
    ) -> Self {
        match kind {
            FunctionKind::Internal(InternalFunction::Date) => match literals.next() {
                None | Some(Literal::Computed) => Effect::Clock,
                Some(Literal::Constant) => Effect::Pure,
                Some(Literal::Text(text)) if Date::parses(text) || Tz::from_str(text).is_err() => {
                    Effect::Pure
                }
                Some(Literal::Text(_)) => Effect::Clock,
            },
            FunctionKind::Deprecated(DeprecatedFunction::Duration) => Effect::Pure,
            FunctionKind::Deprecated(_) => {
                match literals.all(|l| !matches!(l, Literal::Computed | Literal::Text("now"))) {
                    true => Effect::Pure,
                    false => Effect::Clock,
                }
            }
            _ => Self::function(kind),
        }
    }

    pub(crate) fn formats(pattern: &str) -> bool {
        use chrono::TimeZone;
        let Ok(items) = chrono::format::StrftimeItems::new(pattern).parse_to_owned() else {
            return false;
        };
        Tz::UTC.timestamp_opt(0, 0).earliest().is_some_and(|date| {
            let mut out = String::new();
            std::fmt::Write::write_fmt(
                &mut out,
                format_args!("{}", date.format_with_items(items.iter())),
            )
            .is_ok()
        })
    }
}

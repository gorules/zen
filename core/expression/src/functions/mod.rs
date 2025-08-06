pub use crate::functions::date_method::DateMethod;
pub use crate::functions::defs::FunctionTypecheck;
pub use crate::functions::deprecated::DeprecatedFunction;
pub use crate::functions::internal::InternalFunction;
pub use crate::functions::method::{MethodKind, MethodRegistry};
use crate::functions::mf_function::MfFunction;
pub use crate::functions::state_guard::{StateGuard, with_state_async};
pub use crate::functions::registry::FunctionRegistry;

use std::fmt::Display;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

pub  mod arguments;
mod date_method;
pub(crate) mod defs;
mod deprecated;
pub(crate) mod internal;
mod method;
pub(crate) mod registry;
pub mod mf_function;
pub mod state_guard;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum FunctionKind {
    Internal(InternalFunction),
    Deprecated(DeprecatedFunction),
    Closure(ClosureFunction),
    Mf(MfFunction),
}

impl TryFrom<&str> for FunctionKind {
    type Error = strum::ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        InternalFunction::try_from(value)
            .map(FunctionKind::Internal)
            .or_else(|_| DeprecatedFunction::try_from(value).map(FunctionKind::Deprecated))
            .or_else(|_| ClosureFunction::try_from(value).map(FunctionKind::Closure))
            .or_else(|_| MfFunction::try_from(value).map(FunctionKind::Mf))
    }
}

impl Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionKind::Internal(i) => write!(f, "{i}"),
            FunctionKind::Deprecated(d) => write!(f, "{d}"),
            FunctionKind::Closure(c) => write!(f, "{c}"),
            FunctionKind::Mf(m) => write!(f, "{m}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Display, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum ClosureFunction {
    All,
    None,
    Some,
    One,
    Filter,
    Map,
    FlatMap,
    Count,
}

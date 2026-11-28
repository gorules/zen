pub use crate::functions::arguments::Arguments;
pub use crate::functions::date_method::DateMethod;
pub use crate::functions::defs::FunctionTypecheck;
pub use crate::functions::defs::{
    CompositeFunction, FunctionDefinition, FunctionSignature, StaticFunction,
};
pub use crate::functions::deprecated::DeprecatedFunction;
pub use crate::functions::internal::{CallbackSlot, InternalFunction};
pub use crate::functions::method::{MethodKind, MethodRegistry};
pub use crate::functions::registry::{register_host_function, FunctionRegistry, HostFunction};

use std::fmt::Display;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

pub(crate) mod aggregate;
pub mod arguments;
mod date_method;
pub(crate) mod defs;
mod deprecated;
pub(crate) mod internal;
mod method;
pub(crate) mod normalize;
pub(crate) mod registry;
pub mod sketch;
pub(crate) mod stats;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum FunctionKind {
    Internal(InternalFunction),
    Deprecated(DeprecatedFunction),
    Closure(ClosureFunction),
    Host(std::sync::Arc<str>),
}

impl TryFrom<&str> for FunctionKind {
    type Error = strum::ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        InternalFunction::try_from(value)
            .map(FunctionKind::Internal)
            .or_else(|_| DeprecatedFunction::try_from(value).map(FunctionKind::Deprecated))
            .or_else(|_| ClosureFunction::try_from(value).map(FunctionKind::Closure))
            .or_else(|err| match registry::host_function_name(value) {
                Some(name) => Ok(FunctionKind::Host(name)),
                None => Err(err),
            })
    }
}

impl Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionKind::Internal(i) => write!(f, "{i}"),
            FunctionKind::Deprecated(d) => write!(f, "{d}"),
            FunctionKind::Closure(c) => write!(f, "{c}"),
            FunctionKind::Host(name) => write!(f, "{name}"),
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

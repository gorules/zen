pub use crate::functions::arguments::Arguments;
pub use crate::functions::date_method::DateMethod;
pub use crate::functions::defs::{
    CompositeFunction, FunctionDefinition, FunctionSignature, FunctionTypecheck, StaticFunction,
};
pub use crate::functions::deprecated::DeprecatedFunction;
pub use crate::functions::internal::InternalFunction;
pub use crate::functions::method::{MethodKind, MethodRegistry};
pub use crate::functions::registry::{register_host_function, FunctionRegistry, HostFunction};

use std::fmt::Display;
use std::sync::Arc;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

pub mod arguments;
mod date_method;
pub(crate) mod defs;
mod deprecated;
pub(crate) mod internal;
mod method;
pub(crate) mod registry;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum FunctionKind {
    Internal(InternalFunction),
    Deprecated(DeprecatedFunction),
    Closure(ClosureFunction),
    /// A function registered by the host application (see [`register_host_function`]).
    Host(Arc<str>),
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
    // Aggregates over the items, optionally projected and filtered:
    // `sum(arr, #.amount, #.kind == 'in')`. Null projections are skipped.
    Sum,
    Avg,
    Min,
    Max,
    Median,
    Mode,
    Stddev,
    Variance,
    // With a parameter after the projection, then the optional filter:
    // `topK(items as t, t.merchant, 3 [, t.kind == 'in'])`.
    TopK,
    LastN,
    Percentile,
    CountDistinct,
    Unique,
    First,
    Last,
}

impl ClosureFunction {
    /// Callbacks accepted after the array: `(required, maximum)`.
    pub fn callbacks(&self) -> (usize, usize) {
        match self {
            ClosureFunction::All
            | ClosureFunction::None
            | ClosureFunction::Some
            | ClosureFunction::One
            | ClosureFunction::Filter
            | ClosureFunction::Map
            | ClosureFunction::FlatMap => (1, 1),
            ClosureFunction::Count => (0, 1),
            ClosureFunction::Sum
            | ClosureFunction::Avg
            | ClosureFunction::Min
            | ClosureFunction::Max
            | ClosureFunction::Median
            | ClosureFunction::Mode
            | ClosureFunction::Stddev
            | ClosureFunction::Variance => (1, 2),
            ClosureFunction::TopK | ClosureFunction::LastN | ClosureFunction::Percentile => {
                (2, 3)
            }
            ClosureFunction::CountDistinct
            | ClosureFunction::Unique
            | ClosureFunction::First
            | ClosureFunction::Last => (0, 2),
        }
    }

    /// Aggregates: the first callback projects an item, the second filters.
    /// The others take one predicate (or mapping) callback.
    pub fn is_aggregate(&self) -> bool {
        !matches!(
            self,
            ClosureFunction::All
                | ClosureFunction::None
                | ClosureFunction::Some
                | ClosureFunction::One
                | ClosureFunction::Filter
                | ClosureFunction::Map
                | ClosureFunction::FlatMap
                | ClosureFunction::Count
        )
    }

    /// Whether callback `index` (1-based argument position) must return a bool.
    pub fn is_predicate(&self, index: usize) -> bool {
        match self {
            ClosureFunction::Map | ClosureFunction::FlatMap => false,
            c if c.parameter().is_some() => index == 3,
            c if c.is_aggregate() => index == 2,
            _ => index == 1,
        }
    }

    /// The argument position of a plain parameter (a count or fraction),
    /// evaluated once rather than per item.
    pub fn parameter(&self) -> Option<usize> {
        matches!(
            self,
            ClosureFunction::TopK | ClosureFunction::LastN | ClosureFunction::Percentile
        )
        .then_some(2)
    }
}

impl InternalFunction {
    /// The callback form of an array aggregate: `sum([1, 2])` stays internal,
    /// `sum(items, #.amount)` is [`ClosureFunction::Sum`].
    pub fn closure_form(&self) -> Option<ClosureFunction> {
        Some(match self {
            InternalFunction::Sum => ClosureFunction::Sum,
            InternalFunction::Avg => ClosureFunction::Avg,
            InternalFunction::Min => ClosureFunction::Min,
            InternalFunction::Max => ClosureFunction::Max,
            InternalFunction::Median => ClosureFunction::Median,
            InternalFunction::Mode => ClosureFunction::Mode,
            InternalFunction::Stddev => ClosureFunction::Stddev,
            InternalFunction::Variance => ClosureFunction::Variance,
            _ => return None,
        })
    }

    /// The callback form of a built-in that already takes a second argument
    /// (`topK(arr, 3)`): used only with `as` or with three or more
    /// arguments, so every two-argument call stays the built-in.
    pub fn parameterized_closure_form(&self) -> Option<ClosureFunction> {
        Some(match self {
            InternalFunction::TopK => ClosureFunction::TopK,
            InternalFunction::LastN => ClosureFunction::LastN,
            InternalFunction::Percentile => ClosureFunction::Percentile,
            _ => return None,
        })
    }
}

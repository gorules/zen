pub use crate::functions::date_method::DateMethod;
pub use crate::functions::defs::FunctionTypecheck;
pub use crate::functions::deprecated::DeprecatedFunction;
pub use crate::functions::internal::InternalFunction;
pub use crate::functions::method::{MethodKind, MethodRegistry};
pub use crate::functions::registry::FunctionRegistry;

use std::fmt::Display;
use strum_macros::{Display, EnumIter, EnumString, IntoStaticStr};

pub(crate) mod arguments;
mod date_method;
pub(crate) mod defs;
mod deprecated;
pub(crate) mod internal;
mod method;
pub mod moments;
pub(crate) mod registry;
pub mod sketch;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum FunctionKind {
    Internal(InternalFunction),
    Deprecated(DeprecatedFunction),
    Closure(ClosureFunction),
}

impl TryFrom<&str> for FunctionKind {
    type Error = strum::ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        InternalFunction::try_from(value)
            .map(FunctionKind::Internal)
            .or_else(|_| DeprecatedFunction::try_from(value).map(FunctionKind::Deprecated))
            .or_else(|_| ClosureFunction::try_from(value).map(FunctionKind::Closure))
    }
}

impl Display for FunctionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionKind::Internal(i) => write!(f, "{i}"),
            FunctionKind::Deprecated(d) => write!(f, "{d}"),
            FunctionKind::Closure(c) => write!(f, "{c}"),
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
    /// Selectors return an item, or a projection of it.
    ///
    /// - `first(items)`: the first non-null item.
    /// - `first(items, cb)`: per item, a `bool` callback value is a
    ///   condition (`true`: the item is a candidate, `false`: skipped); any
    ///   other non-null value is the projection (as in
    ///   `first(items, value, cond)`); null is skipped. The first candidate
    ///   wins. Analysis follows the same rule: a `bool` callback types the
    ///   result as the item, else as the projection.
    /// - `first(items, value, cond)`: the first non-null `value` at a
    ///   matching item, even when `value` is a `bool`.
    ///
    /// `last` is the same from the end.
    First,
    Last,
    /// The item (or a projection of it) with the greatest (smallest) `by`,
    /// a number or date; ties go to the last, a null `by` is skipped:
    ///
    /// - `argMax(items as t, t.amount)`: the item;
    /// - `argMax(items as t, t.merchant, t.amount [, t.kind == 'in'])`: its
    ///   `merchant`.
    ///
    /// Three arguments are always `(value, by)`: a condition on the item
    /// form needs the four-argument form (`argMax(items as t, t, t.amount,
    /// cond)`) or `argMax(filter(items, cond), #.amount)`.
    ArgMax,
    ArgMin,
    Skew,
    Kurtosis,
    CountDistinctApprox,
    // With a parameter (the fraction) like `percentile`.
    PercentileApprox,
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
            ClosureFunction::TopK
            | ClosureFunction::LastN
            | ClosureFunction::Percentile
            | ClosureFunction::PercentileApprox => (2, 3),
            ClosureFunction::ArgMax | ClosureFunction::ArgMin => (1, 3),
            ClosureFunction::CountDistinct
            | ClosureFunction::Unique
            | ClosureFunction::First
            | ClosureFunction::Last
            | ClosureFunction::Skew
            | ClosureFunction::Kurtosis
            | ClosureFunction::CountDistinctApprox => (0, 2),
        }
    }

    /// How many projections an aggregate reads per item: two for `argMax`
    /// and `argMin` (the value, then what ranks it), else one. With a
    /// single callback (`argMax(items, #.amount)`) the value is the item.
    pub fn projections(&self) -> usize {
        match self {
            ClosureFunction::ArgMax | ClosureFunction::ArgMin => 2,
            _ => 1,
        }
    }

    /// The argument position of an aggregate's optional filter: after the
    /// projections and the parameter.
    pub fn filter_position(&self) -> usize {
        1 + self.projections() + usize::from(self.parameter().is_some())
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
            c if c.is_aggregate() => index == c.filter_position(),
            _ => index == 1,
        }
    }

    /// The argument position of a plain parameter (a count or fraction),
    /// evaluated once rather than per item.
    pub fn parameter(&self) -> Option<usize> {
        matches!(
            self,
            ClosureFunction::TopK
                | ClosureFunction::LastN
                | ClosureFunction::Percentile
                | ClosureFunction::PercentileApprox
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
    /// (`topK(arr, 3)`): used only with three or more arguments, so every
    /// two-argument call stays the built-in, with or without an alias
    /// (`lastN(items as t, 5)` is `lastN(items, 5)`).
    pub fn parameterized_closure_form(&self) -> Option<ClosureFunction> {
        Some(match self {
            InternalFunction::TopK => ClosureFunction::TopK,
            InternalFunction::LastN => ClosureFunction::LastN,
            InternalFunction::Percentile => ClosureFunction::Percentile,
            InternalFunction::PercentileApprox => ClosureFunction::PercentileApprox,
            _ => return None,
        })
    }
}

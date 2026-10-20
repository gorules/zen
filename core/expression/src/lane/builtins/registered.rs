//! Functions whose one implementation is the registry's definition: the stack
//! VM calls it from `CallFunction`, the lane drives the same definition on
//! the lane's values. One kernel, two drivers: values, scales and error texts
//! are the VM's by construction (the lane's `call` wraps a failure as the VM
//! does, ``Function `x` failed: …``).
//!
//! These are the feature-store aggregates (statistics, sketches, selectors,
//! the callback aggregates' final step) and the functions that grew with
//! them (`join`, `mode` over strings). A function that later earns a native
//! lane kernel moves to its family; the definition stays the oracle.

use crate::functions::arguments::Arguments;
use crate::functions::{ClosureFunction as C, FunctionKind, FunctionRegistry, InternalFunction as F};
use crate::lane::builtins::{Arg, Builtin, Out, Outcome};
use crate::Variable;

pub(crate) struct Registered;

/// Calls the registry's definition of `kind` on `args`. `None` only if the
/// registry has no definition (then the call fails as "not found").
fn drive(kind: &FunctionKind, args: &[Arg]) -> Outcome {
    let definition = FunctionRegistry::get_definition(kind)?;
    let values: Vec<Variable> = args.iter().map(Arg::variable).collect();
    Some(
        definition
            .call(Arguments(&values))
            .map(Out::Var)
            .map_err(|error| error.to_string()),
    )
}

macro_rules! registered {
    ($($name:ident = $kind:expr;)*) => {
        impl Registered {
            $(
                pub(crate) const $name: Builtin = Builtin {
                    overloads: &[{
                        fn overload(args: &[Arg]) -> Outcome {
                            drive(&$kind, args)
                        }
                        overload
                    }],
                    // The definition checks its own arguments (as in the VM).
                    fail: |_| String::new(),
                };
            )*
        }
    };
}

registered! {
    JOIN = FunctionKind::Internal(F::Join);
    MODE = FunctionKind::Internal(F::Mode);
    STDDEV = FunctionKind::Internal(F::Stddev);
    VARIANCE = FunctionKind::Internal(F::Variance);
    PERCENTILE = FunctionKind::Internal(F::Percentile);
    PERCENTILE_APPROX = FunctionKind::Internal(F::PercentileApprox);
    TOP_K = FunctionKind::Internal(F::TopK);
    LAST_N = FunctionKind::Internal(F::LastN);

    AGG_SUM = FunctionKind::Closure(C::Sum);
    AGG_AVG = FunctionKind::Closure(C::Avg);
    AGG_MIN = FunctionKind::Closure(C::Min);
    AGG_MAX = FunctionKind::Closure(C::Max);
    AGG_MEDIAN = FunctionKind::Closure(C::Median);
    AGG_MODE = FunctionKind::Closure(C::Mode);
    AGG_STDDEV = FunctionKind::Closure(C::Stddev);
    AGG_VARIANCE = FunctionKind::Closure(C::Variance);
    AGG_TOP_K = FunctionKind::Closure(C::TopK);
    AGG_LAST_N = FunctionKind::Closure(C::LastN);
    AGG_PERCENTILE = FunctionKind::Closure(C::Percentile);
    AGG_PERCENTILE_APPROX = FunctionKind::Closure(C::PercentileApprox);
    AGG_COUNT_DISTINCT = FunctionKind::Closure(C::CountDistinct);
    AGG_COUNT_DISTINCT_APPROX = FunctionKind::Closure(C::CountDistinctApprox);
    AGG_UNIQUE = FunctionKind::Closure(C::Unique);
    AGG_FIRST = FunctionKind::Closure(C::First);
    AGG_LAST = FunctionKind::Closure(C::Last);
    AGG_ARG_MAX = FunctionKind::Closure(C::ArgMax);
    AGG_ARG_MIN = FunctionKind::Closure(C::ArgMin);
    AGG_SKEW = FunctionKind::Closure(C::Skew);
    AGG_KURTOSIS = FunctionKind::Closure(C::Kurtosis);
}

impl Registered {
    pub(crate) const HOST: Builtin = Builtin {
        overloads: &[],
        fail: |_| String::from("host function called without its name"),
    };

    pub(crate) fn host(kind: &FunctionKind, args: &[Arg]) -> Result<Out, String> {
        drive(kind, args).unwrap_or_else(|| Err(format!("Function `{kind}` not found")))
    }

    /// The final step of a callback aggregate (`sum(items, #.v)` …): the
    /// collected values (and the parameter) in, the aggregate out. `None`
    /// for the callbacks that are no aggregate (`map`, `filter`, `count`…).
    pub(crate) fn aggregate(kind: C) -> Option<&'static Builtin> {
        Some(match kind {
            C::Sum => &Self::AGG_SUM,
            C::Avg => &Self::AGG_AVG,
            C::Min => &Self::AGG_MIN,
            C::Max => &Self::AGG_MAX,
            C::Median => &Self::AGG_MEDIAN,
            C::Mode => &Self::AGG_MODE,
            C::Stddev => &Self::AGG_STDDEV,
            C::Variance => &Self::AGG_VARIANCE,
            C::TopK => &Self::AGG_TOP_K,
            C::LastN => &Self::AGG_LAST_N,
            C::Percentile => &Self::AGG_PERCENTILE,
            C::PercentileApprox => &Self::AGG_PERCENTILE_APPROX,
            C::CountDistinct => &Self::AGG_COUNT_DISTINCT,
            C::CountDistinctApprox => &Self::AGG_COUNT_DISTINCT_APPROX,
            C::Unique => &Self::AGG_UNIQUE,
            C::First => &Self::AGG_FIRST,
            C::Last => &Self::AGG_LAST,
            C::ArgMax => &Self::AGG_ARG_MAX,
            C::ArgMin => &Self::AGG_ARG_MIN,
            C::Skew => &Self::AGG_SKEW,
            C::Kurtosis => &Self::AGG_KURTOSIS,
            C::All
            | C::None
            | C::Some
            | C::One
            | C::Filter
            | C::Map
            | C::FlatMap
            | C::Count => return None,
        })
    }
}

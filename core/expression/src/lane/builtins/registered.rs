use crate::functions::arguments::Arguments;
use crate::functions::{FunctionKind, FunctionRegistry, InternalFunction as F};
use crate::lane::builtins::{Arg, Builtin, Out, Outcome};
use crate::variable::Variable;

pub(crate) struct Registered;

fn drive(function: F, args: &[Arg]) -> Outcome {
    let definition = FunctionRegistry::get_definition(&FunctionKind::Internal(function))?;
    let values: Vec<Variable> = args.iter().map(Arg::variable).collect();
    Some(
        definition
            .call(Arguments(&values))
            .map(Out::Var)
            .map_err(|error| error.to_string()),
    )
}

macro_rules! registered {
    ($($name:ident = $function:ident;)*) => {
        impl Registered {
            $(
                pub(crate) const $name: Builtin = Builtin {
                    overloads: &[|_, args| drive(F::$function, args)],
                    fail: |_| String::new(),
                };
            )*

            pub(crate) fn of(function: &F) -> Option<&'static Builtin> {
                match function {
                    $(F::$function => Some(&Self::$name),)*
                    _ => None,
                }
            }
        }
    };
}

registered! {
    JOIN = Join;
    REPLACE = Replace;
    REPLACE_ALL = ReplaceAll;
    UNACCENT = Unaccent;
    SQUISH = Squish;
    TOKEN_SORT = TokenSort;
    NAME_KEY = NameKey;
    SOUNDEX = Soundex;
    JARO_WINKLER = JaroWinkler;
    TOKEN_SET_SIMILARITY = TokenSetSimilarity;
    TAKE = Take;
    TAKE_LAST = TakeLast;
    FIND = Find;
    FIND_LAST = FindLast;
    FIND_INDEX = FindIndex;
    FIND_LAST_INDEX = FindLastIndex;
    SORT = Sort;
    SORT_BY = SortBy;
    IS_EMPTY = IsEmpty;
    STDDEV = Stddev;
    VARIANCE = Variance;
    STDDEV_POP = StddevPop;
    VARIANCE_POP = VariancePop;
    SKEW = Skew;
    KURTOSIS = Kurtosis;
    PERCENTILE = Percentile;
    PERCENTILE_APPROX = PercentileApprox;
    COUNT_DISTINCT = CountDistinct;
    COUNT_DISTINCT_APPROX = CountDistinctApprox;
    DISTINCT = Distinct;
    TOP_K = TopK;
    FIRST = First;
    LAST = Last;
    ARG_MAX = ArgMax;
    ARG_MIN = ArgMin;
    AGGREGATE_SUM = AggregateSum;
    AGGREGATE_AVG = AggregateAvg;
    AGGREGATE_MIN = AggregateMin;
    AGGREGATE_MAX = AggregateMax;
    AGGREGATE_MEDIAN = AggregateMedian;
    AGGREGATE_MODE = AggregateMode;
}

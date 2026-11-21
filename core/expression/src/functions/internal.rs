use crate::functions::defs::{
    CompositeFunction, FunctionDefinition, FunctionSignature, StaticFunction,
};
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use strum_macros::{EnumIter, EnumString, IntoStaticStr};

#[derive(Debug, PartialEq, Eq, Hash, EnumString, EnumIter, IntoStaticStr, Clone, Copy)]
#[strum(serialize_all = "camelCase")]
pub enum InternalFunction {
    // General
    Len,
    Contains,
    Flatten,
    Merge,
    MergeDeep,

    // String
    Upper,
    Lower,
    Trim,
    StartsWith,
    EndsWith,
    Matches,
    Extract,
    FuzzyMatch,
    Split,
    Join,
    Replace,
    ReplaceAll,
    Unaccent,
    Squish,
    TokenSort,
    NameKey,
    Soundex,
    JaroWinkler,
    TokenSetSimilarity,

    // Math
    Abs,
    Sum,
    Avg,
    Min,
    Max,
    Rand,
    Median,
    Mode,
    Floor,
    Ceil,
    Round,
    Trunc,
    Stddev,
    Variance,
    StddevPop,
    VariancePop,
    Skew,
    Kurtosis,
    Percentile,
    PercentileApprox,

    // Aggregate
    #[strum(to_string = "countDistinct", serialize = "count_distinct")]
    CountDistinct,
    CountDistinctApprox,
    #[strum(to_string = "distinct", serialize = "unique")]
    Distinct,
    TopK,
    First,
    Last,
    #[strum(to_string = "argMax", serialize = "maxBy")]
    ArgMax,
    #[strum(to_string = "argMin", serialize = "minBy")]
    ArgMin,
    Take,
    TakeLast,
    Find,
    FindLast,
    FindIndex,
    FindLastIndex,
    Sort,
    SortBy,
    IsEmpty,
    #[strum(serialize = "@sum")]
    AggregateSum,
    #[strum(serialize = "@avg")]
    AggregateAvg,
    #[strum(serialize = "@min")]
    AggregateMin,
    #[strum(serialize = "@max")]
    AggregateMax,
    #[strum(serialize = "@median")]
    AggregateMedian,
    #[strum(serialize = "@mode")]
    AggregateMode,

    // Type
    IsNumeric,
    String,
    Number,
    Bool,
    Type,

    // Map
    Keys,
    Values,

    #[strum(serialize = "d")]
    Date,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum CallbackSlot {
    Callback,
    Parameter,
}

impl InternalFunction {
    pub fn callback_slots(&self, arguments: usize) -> Option<&'static [CallbackSlot]> {
        use CallbackSlot::{Callback as C, Parameter as P};
        use InternalFunction as IF;
        Some(match (self, arguments) {
            (
                IF::Sum
                | IF::Avg
                | IF::Min
                | IF::Max
                | IF::Median
                | IF::Mode
                | IF::Stddev
                | IF::Variance
                | IF::StddevPop
                | IF::VariancePop
                | IF::Skew
                | IF::Kurtosis
                | IF::CountDistinct
                | IF::CountDistinctApprox
                | IF::Distinct
                | IF::First
                | IF::Last,
                2,
            ) => &[C],
            (
                IF::Sum
                | IF::Avg
                | IF::Min
                | IF::Max
                | IF::Median
                | IF::Mode
                | IF::Stddev
                | IF::Variance
                | IF::StddevPop
                | IF::VariancePop
                | IF::Skew
                | IF::Kurtosis
                | IF::CountDistinct
                | IF::CountDistinctApprox
                | IF::Distinct
                | IF::First
                | IF::Last,
                3,
            ) => &[C, C],
            (IF::Percentile | IF::PercentileApprox | IF::TopK, 3) => &[C, P],
            (IF::Percentile | IF::PercentileApprox | IF::TopK, 4) => &[C, P, C],
            (IF::Find | IF::FindLast | IF::FindIndex | IF::FindLastIndex, 2) => &[C],
            (IF::SortBy, 2) => &[C],
            (IF::SortBy, 3) => &[C, P],
            (IF::ArgMax | IF::ArgMin, 2) => &[C],
            (IF::ArgMax | IF::ArgMin, 3) => &[C, C],
            (IF::ArgMax | IF::ArgMin, 4) => &[C, C, C],
            _ => return None,
        })
    }

    pub fn aggregated(&self) -> Option<InternalFunction> {
        use InternalFunction as IF;
        Some(match self {
            IF::Sum => IF::AggregateSum,
            IF::Avg => IF::AggregateAvg,
            IF::Min => IF::AggregateMin,
            IF::Max => IF::AggregateMax,
            IF::Median => IF::AggregateMedian,
            IF::Mode => IF::AggregateMode,
            _ => return None,
        })
    }

    pub fn aggregate_of(&self) -> Option<InternalFunction> {
        use InternalFunction as IF;
        Some(match self {
            IF::AggregateSum => IF::Sum,
            IF::AggregateAvg => IF::Avg,
            IF::AggregateMin => IF::Min,
            IF::AggregateMax => IF::Max,
            IF::AggregateMedian => IF::Median,
            IF::AggregateMode => IF::Mode,
            _ => return None,
        })
    }

    pub fn is_hidden(&self) -> bool {
        self.aggregate_of().is_some()
    }

    pub fn name(&self) -> &'static str {
        match self.aggregate_of() {
            Some(base) => base.into(),
            None => self.into(),
        }
    }
}

impl Display for InternalFunction {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl From<&InternalFunction> for Rc<dyn FunctionDefinition> {
    fn from(value: &InternalFunction) -> Self {
        use crate::variable::VariableType as VT;
        use InternalFunction as IF;

        let s: Rc<dyn FunctionDefinition> = match value {
            IF::Len => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::len),
                signatures: vec![
                    FunctionSignature::single(VT::String, VT::Number),
                    FunctionSignature::single(VT::Any.array(), VT::Number),
                ],
            }),

            IF::Contains => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::contains),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::String, VT::String],
                        return_type: VT::Bool,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Any.array(), VT::Any],
                        return_type: VT::Bool,
                    },
                ],
            }),

            IF::Flatten => Rc::new(StaticFunction {
                implementation: Rc::new(imp::flatten),
                signature: FunctionSignature::single(VT::Any.array(), VT::Any.array()),
            }),

            IF::Merge => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::merge),
                signatures: vec![
                    FunctionSignature::single(VT::Any.array(), VT::Any.array()),
                    FunctionSignature::single(
                        VT::Object(Default::default()).array(),
                        VT::Object(Default::default()),
                    ),
                ],
            }),

            IF::MergeDeep => Rc::new(StaticFunction {
                implementation: Rc::new(imp::merge_deep),
                signature: FunctionSignature::single(
                    VT::Object(Default::default()).array(),
                    VT::Object(Default::default()),
                ),
            }),

            IF::Upper => Rc::new(StaticFunction {
                implementation: Rc::new(imp::upper),
                signature: FunctionSignature::single(VT::String, VT::String),
            }),

            IF::Lower => Rc::new(StaticFunction {
                implementation: Rc::new(imp::lower),
                signature: FunctionSignature::single(VT::String, VT::String),
            }),

            IF::Trim => Rc::new(StaticFunction {
                implementation: Rc::new(imp::trim),
                signature: FunctionSignature::single(VT::String, VT::String),
            }),

            IF::StartsWith => Rc::new(StaticFunction {
                implementation: Rc::new(imp::starts_with),
                signature: FunctionSignature {
                    parameters: vec![VT::String, VT::String],
                    return_type: VT::Bool,
                },
            }),

            IF::EndsWith => Rc::new(StaticFunction {
                implementation: Rc::new(imp::ends_with),
                signature: FunctionSignature {
                    parameters: vec![VT::String, VT::String],
                    return_type: VT::Bool,
                },
            }),

            IF::Matches => Rc::new(StaticFunction {
                implementation: Rc::new(imp::matches),
                signature: FunctionSignature {
                    parameters: vec![VT::String, VT::String],
                    return_type: VT::Bool,
                },
            }),

            IF::Extract => Rc::new(StaticFunction {
                implementation: Rc::new(imp::extract),
                signature: FunctionSignature {
                    parameters: vec![VT::String, VT::String],
                    return_type: VT::String.array(),
                },
            }),

            IF::Split => Rc::new(StaticFunction {
                implementation: Rc::new(imp::split),
                signature: FunctionSignature {
                    parameters: vec![VT::String, VT::String],
                    return_type: VT::String.array(),
                },
            }),

            IF::Join => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::join),
                signatures: vec![
                    FunctionSignature::single(VT::Any.array(), VT::String),
                    FunctionSignature {
                        parameters: vec![VT::Any.array(), VT::String],
                        return_type: VT::String,
                    },
                ],
            }),

            IF::Replace | IF::ReplaceAll => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::Replace => imp::replace,
                    _ => imp::replace_all,
                }),
                signature: FunctionSignature {
                    parameters: vec![VT::String, VT::String, VT::String],
                    return_type: VT::String,
                },
            }),

            IF::Unaccent | IF::Squish | IF::TokenSort | IF::NameKey | IF::Soundex => {
                Rc::new(StaticFunction {
                    implementation: Rc::new(match value {
                        IF::Unaccent => imp::unaccent,
                        IF::Squish => imp::squish,
                        IF::TokenSort => imp::token_sort,
                        IF::NameKey => imp::name_key,
                        _ => imp::soundex,
                    }),
                    signature: FunctionSignature::single(VT::String, VT::String),
                })
            }

            IF::JaroWinkler | IF::TokenSetSimilarity => Rc::new(CompositeFunction {
                implementation: Rc::new(match value {
                    IF::JaroWinkler => imp::jaro_winkler,
                    _ => imp::token_set_similarity,
                }),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::String, VT::String],
                        return_type: VT::Number,
                    },
                    FunctionSignature {
                        parameters: vec![VT::String.array(), VT::String],
                        return_type: VT::Number.array(),
                    },
                ],
            }),

            IF::FuzzyMatch => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::fuzzy_match),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::String, VT::String],
                        return_type: VT::Number,
                    },
                    FunctionSignature {
                        parameters: vec![VT::String.array(), VT::String],
                        return_type: VT::Number.array(),
                    },
                ],
            }),

            IF::Abs => Rc::new(StaticFunction {
                implementation: Rc::new(imp::abs),
                signature: FunctionSignature::single(VT::Number, VT::Number),
            }),

            IF::Rand => Rc::new(StaticFunction {
                implementation: Rc::new(imp::rand),
                signature: FunctionSignature::single(VT::Number, VT::Number),
            }),

            IF::Floor => Rc::new(StaticFunction {
                implementation: Rc::new(imp::floor),
                signature: FunctionSignature::single(VT::Number, VT::Number),
            }),

            IF::Ceil => Rc::new(StaticFunction {
                implementation: Rc::new(imp::ceil),
                signature: FunctionSignature::single(VT::Number, VT::Number),
            }),

            IF::Round => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::round),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::Number],
                        return_type: VT::Number,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Number, VT::Number],
                        return_type: VT::Number,
                    },
                ],
            }),

            IF::Trunc => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::trunc),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![VT::Number],
                        return_type: VT::Number,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Number, VT::Number],
                        return_type: VT::Number,
                    },
                ],
            }),

            IF::Sum => Rc::new(StaticFunction {
                implementation: Rc::new(imp::sum),
                signature: FunctionSignature::single(VT::Number.array(), VT::Number),
            }),

            IF::Avg => Rc::new(StaticFunction {
                implementation: Rc::new(imp::avg),
                signature: FunctionSignature::single(VT::Number.array(), VT::Number),
            }),

            IF::Min => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::min),
                signatures: vec![
                    FunctionSignature::single(VT::Number.array(), VT::Number),
                    FunctionSignature::single(VT::Date.array(), VT::Date),
                ],
            }),

            IF::Max => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::max),
                signatures: vec![
                    FunctionSignature::single(VT::Number.array(), VT::Number),
                    FunctionSignature::single(VT::Date.array(), VT::Date),
                ],
            }),

            IF::Median => Rc::new(StaticFunction {
                implementation: Rc::new(imp::median),
                signature: FunctionSignature::single(VT::Number.array(), VT::Number),
            }),

            IF::Mode => Rc::new(StaticFunction {
                implementation: Rc::new(imp::mode),
                signature: FunctionSignature::single(VT::Number.array(), VT::Number),
            }),

            IF::Stddev
            | IF::Variance
            | IF::StddevPop
            | IF::VariancePop
            | IF::Skew
            | IF::Kurtosis
            | IF::AggregateAvg
            | IF::AggregateMedian => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::Stddev => imp::stddev,
                    IF::Variance => imp::variance,
                    IF::StddevPop => imp::stddev_pop,
                    IF::VariancePop => imp::variance_pop,
                    IF::Skew => imp::skew,
                    IF::Kurtosis => imp::kurtosis,
                    IF::AggregateAvg => imp::aggregate_avg,
                    _ => imp::aggregate_median,
                }),
                signature: FunctionSignature::single(
                    VT::Nullable(Rc::new(VT::Number)).array(),
                    VT::Nullable(Rc::new(VT::Number)),
                ),
            }),

            IF::Percentile | IF::PercentileApprox => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::Percentile => imp::percentile,
                    _ => imp::percentile_approx,
                }),
                signature: FunctionSignature {
                    parameters: vec![VT::Nullable(Rc::new(VT::Number)).array(), VT::Number],
                    return_type: VT::Nullable(Rc::new(VT::Number)),
                },
            }),

            IF::AggregateSum => Rc::new(StaticFunction {
                implementation: Rc::new(imp::aggregate_sum),
                signature: FunctionSignature::single(
                    VT::Nullable(Rc::new(VT::Number)).array(),
                    VT::Number,
                ),
            }),

            IF::AggregateMin | IF::AggregateMax => Rc::new(CompositeFunction {
                implementation: Rc::new(match value {
                    IF::AggregateMin => imp::aggregate_min,
                    _ => imp::aggregate_max,
                }),
                signatures: vec![
                    FunctionSignature::single(
                        VT::Nullable(Rc::new(VT::Number)).array(),
                        VT::Nullable(Rc::new(VT::Number)),
                    ),
                    FunctionSignature::single(
                        VT::Nullable(Rc::new(VT::Date)).array(),
                        VT::Nullable(Rc::new(VT::Date)),
                    ),
                ],
            }),

            IF::AggregateMode => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::aggregate_mode),
                signatures: vec![
                    FunctionSignature::single(
                        VT::Nullable(Rc::new(VT::Number)).array(),
                        VT::Nullable(Rc::new(VT::Number)),
                    ),
                    FunctionSignature::single(
                        VT::Nullable(Rc::new(VT::String)).array(),
                        VT::Nullable(Rc::new(VT::String)),
                    ),
                ],
            }),

            IF::CountDistinct | IF::CountDistinctApprox => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::CountDistinct => imp::count_distinct,
                    _ => imp::count_distinct_approx,
                }),
                signature: FunctionSignature::single(VT::Any.array(), VT::Number),
            }),

            IF::Distinct => Rc::new(StaticFunction {
                implementation: Rc::new(imp::distinct),
                signature: FunctionSignature::single(VT::Any.array(), VT::Any.array()),
            }),

            IF::TopK => Rc::new(StaticFunction {
                implementation: Rc::new(imp::top_k),
                signature: FunctionSignature {
                    parameters: vec![VT::Any.array(), VT::Number],
                    return_type: VT::Any.array(),
                },
            }),

            IF::First | IF::Last | IF::ArgMax | IF::ArgMin => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::First => imp::first,
                    IF::Last => imp::last,
                    IF::ArgMax => imp::arg_max,
                    _ => imp::arg_min,
                }),
                signature: FunctionSignature::single(VT::Any.array(), VT::Any),
            }),

            IF::Take | IF::TakeLast => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::Take => imp::take,
                    _ => imp::take_last,
                }),
                signature: FunctionSignature {
                    parameters: vec![VT::Any.array(), VT::Number],
                    return_type: VT::Any.array(),
                },
            }),

            IF::Find | IF::FindLast => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::Find => imp::first,
                    _ => imp::last,
                }),
                signature: FunctionSignature::single(VT::Any.array(), VT::Any),
            }),

            IF::FindIndex | IF::FindLastIndex => Rc::new(StaticFunction {
                implementation: Rc::new(match value {
                    IF::FindIndex => imp::find_index,
                    _ => imp::find_last_index,
                }),
                signature: FunctionSignature::single(VT::Any.array(), VT::Number),
            }),

            IF::Sort | IF::SortBy => Rc::new(CompositeFunction {
                implementation: Rc::new(match value {
                    IF::Sort => imp::sort,
                    _ => imp::sort_by,
                }),
                signatures: vec![
                    FunctionSignature::single(VT::Any.array(), VT::Any.array()),
                    FunctionSignature {
                        parameters: vec![VT::Any.array(), VT::String],
                        return_type: VT::Any.array(),
                    },
                ],
            }),

            IF::IsEmpty => Rc::new(StaticFunction {
                implementation: Rc::new(imp::is_empty),
                signature: FunctionSignature::single(VT::Any, VT::Bool),
            }),

            IF::Type => Rc::new(StaticFunction {
                implementation: Rc::new(imp::to_type),
                signature: FunctionSignature::single(VT::Any, VT::String),
            }),

            IF::String => Rc::new(StaticFunction {
                implementation: Rc::new(imp::to_string),
                signature: FunctionSignature::single(VT::Any, VT::String),
            }),

            IF::Bool => Rc::new(StaticFunction {
                implementation: Rc::new(imp::to_bool),
                signature: FunctionSignature::single(VT::Any, VT::Bool),
            }),

            IF::IsNumeric => Rc::new(StaticFunction {
                implementation: Rc::new(imp::is_numeric),
                signature: FunctionSignature::single(VT::Any, VT::Bool),
            }),

            IF::Number => Rc::new(StaticFunction {
                implementation: Rc::new(imp::to_number),
                signature: FunctionSignature::single(VT::Any, VT::Number),
            }),

            IF::Keys => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::keys),
                signatures: vec![
                    FunctionSignature::single(VT::Object(Default::default()), VT::String.array()),
                    FunctionSignature::single(VT::Any.array(), VT::Number.array()),
                ],
            }),

            IF::Values => Rc::new(StaticFunction {
                implementation: Rc::new(imp::values),
                signature: FunctionSignature::single(
                    VT::Object(Default::default()),
                    VT::Any.array(),
                ),
            }),

            IF::Date => Rc::new(CompositeFunction {
                implementation: Rc::new(imp::date),
                signatures: vec![
                    FunctionSignature {
                        parameters: vec![],
                        return_type: VT::Date,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Any],
                        return_type: VT::Date,
                    },
                    FunctionSignature {
                        parameters: vec![VT::Any, VT::String],
                        return_type: VT::Date,
                    },
                ],
            }),
        };

        s
    }
}

pub(crate) mod imp {
    use crate::functions::aggregate;
    use crate::functions::arguments::Arguments;
    use crate::functions::normalize;
    use crate::vm::date::DynamicVariableExt;
    use crate::vm::VmDate;
    use crate::{Variable as V, Variable};
    use anyhow::{anyhow, Context};
    use chrono_tz::Tz;
    #[cfg(not(feature = "regex-lite"))]
    use regex::Regex;
    #[cfg(feature = "regex-lite")]
    use regex_lite::Regex;
    use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
    use rust_decimal::{Decimal, RoundingStrategy};
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::str::FromStr;

    fn __internal_number_array(args: &Arguments, pos: usize) -> anyhow::Result<Vec<Decimal>> {
        let a = args.array(pos)?;
        let arr = a.borrow();

        arr.iter()
            .map(|v| v.as_number())
            .collect::<Option<Vec<_>>>()
            .context("Expected a number array")
    }

    enum Either<A, B> {
        Left(A),
        Right(B),
    }

    fn __internal_number_or_date_array(
        args: &Arguments,
        pos: usize,
    ) -> anyhow::Result<Either<Vec<Decimal>, Vec<VmDate>>> {
        let a = args.array(pos)?;
        let arr = a.borrow();

        let is_number = arr.first().map(|v| v.as_number()).flatten().is_some();
        if is_number {
            Ok(Either::Left(
                arr.iter()
                    .map(|v| v.as_number())
                    .collect::<Option<Vec<_>>>()
                    .context("Expected a number array")?,
            ))
        } else {
            Ok(Either::Right(
                arr.iter()
                    .map(|v| match v {
                        Variable::Dynamic(d) => d.as_date().cloned(),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .context("Expected a number array")?,
            ))
        }
    }

    pub fn starts_with(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        let b = args.str(1)?;

        Ok(V::Bool(a.starts_with(b)))
    }

    pub fn ends_with(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        let b = args.str(1)?;

        Ok(V::Bool(a.ends_with(b)))
    }

    pub fn matches(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        let b = args.str(1)?;

        let regex = Regex::new(b.as_ref()).context("Invalid regular expression")?;

        Ok(V::Bool(regex.is_match(a.as_ref())))
    }

    pub fn upper(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        Ok(V::String(a.to_uppercase().into()))
    }

    pub fn lower(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        Ok(V::String(a.to_lowercase().into()))
    }

    pub fn trim(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        Ok(V::String(a.trim().into()))
    }

    pub fn extract(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        let b = args.str(1)?;

        let regex = Regex::new(b.as_ref()).context("Invalid regular expression")?;

        let captures = regex
            .captures(a.as_ref())
            .map(|capture| {
                capture
                    .iter()
                    .map(|c| c.map(|c| c.as_str()))
                    .filter_map(|c| c)
                    .map(|s| V::String((s).into()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(V::from_array(captures))
    }

    pub fn split(args: Arguments) -> anyhow::Result<V> {
        let a = args.str(0)?;
        let b = args.str(1)?;

        let arr = Vec::from_iter(
            a.split(b)
                .into_iter()
                .map(|s| V::String(s.to_string().into())),
        );

        Ok(V::from_array(arr))
    }

    pub fn join(args: Arguments) -> anyhow::Result<V> {
        let array = args.array(0)?;
        let separator = args.ostr(1)?.unwrap_or(",");
        let items = array.borrow();
        let mut out = String::new();
        let mut first = true;
        for item in items.iter() {
            let text: std::borrow::Cow<str> = match item {
                V::Null => continue,
                V::String(s) => std::borrow::Cow::Borrowed(s.as_str()),
                V::Number(n) => n.normalize().to_string().into(),
                V::Bool(b) => b.to_string().into(),
                V::Dynamic(d) if d.as_date().is_some() => d.to_string().into(),
                other => return Err(anyhow!("Cannot join type {}", other.type_name())),
            };
            if !first {
                out.push_str(separator);
            }
            first = false;
            out.push_str(&text);
        }

        Ok(V::String(out.into()))
    }

    fn regex_replace(args: Arguments, all: bool) -> anyhow::Result<V> {
        let a = args.str(0)?;
        let pattern = args.str(1)?;
        let with = args.str(2)?;
        let regex = Regex::new(pattern).context("Invalid regular expression")?;
        let out = match all {
            true => regex.replace_all(a, with),
            false => regex.replace(a, with),
        };
        Ok(V::String(out.as_ref().into()))
    }

    pub fn replace(args: Arguments) -> anyhow::Result<V> {
        regex_replace(args, false)
    }

    pub fn replace_all(args: Arguments) -> anyhow::Result<V> {
        regex_replace(args, true)
    }

    fn text_map(args: Arguments, f: fn(&str) -> String) -> anyhow::Result<V> {
        let a = args.str(0)?;
        Ok(V::String(f(a).into()))
    }

    pub fn unaccent(args: Arguments) -> anyhow::Result<V> {
        text_map(args, normalize::unaccent)
    }

    pub fn squish(args: Arguments) -> anyhow::Result<V> {
        text_map(args, normalize::squish)
    }

    pub fn token_sort(args: Arguments) -> anyhow::Result<V> {
        text_map(args, normalize::token_sort)
    }

    pub fn name_key(args: Arguments) -> anyhow::Result<V> {
        text_map(args, normalize::name_key)
    }

    pub fn soundex(args: Arguments) -> anyhow::Result<V> {
        text_map(args, normalize::soundex)
    }

    fn similarity(args: Arguments, f: fn(&str, &str) -> f64) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let b = args.str(1)?;
        let score = |x: &str| V::Number(Decimal::from_f64(f(x, b)).unwrap_or(dec!(0)));
        match (a, a.as_str()) {
            (_, Some(a)) => Ok(score(a)),
            (V::Array(items), _) => {
                let items = items.borrow();
                let mut out = Vec::with_capacity(items.len());
                for v in items.iter() {
                    out.push(score(v.as_str().context("Expected string array")?));
                }
                Ok(V::from_array(out))
            }
            _ => Err(anyhow!(
                "Similarity not available for type {}",
                a.type_name()
            )),
        }
    }

    pub fn jaro_winkler(args: Arguments) -> anyhow::Result<V> {
        similarity(args, normalize::jaro_winkler)
    }

    pub fn token_set_similarity(args: Arguments) -> anyhow::Result<V> {
        similarity(args, normalize::token_set_similarity)
    }

    pub fn flatten(args: Arguments) -> anyhow::Result<V> {
        let a = args.array(0)?;

        let arr = a.borrow();
        let mut flat_arr = Vec::with_capacity(arr.len());
        arr.iter().for_each(|v| match v {
            V::Array(b) => {
                let arr = b.borrow();
                arr.iter().for_each(|v| flat_arr.push(v.clone()))
            }
            _ => flat_arr.push(v.clone()),
        });

        Ok(V::from_array(flat_arr))
    }

    pub fn merge(args: Arguments) -> anyhow::Result<V> {
        let a = args.array(0)?;
        let arr = a.borrow();

        let Some(first) = arr.iter().find(|item| !matches!(item, V::Null)) else {
            return Ok(V::empty_object());
        };

        let capacity = arr
            .iter()
            .map(|item| match item {
                V::Object(obj) => obj.borrow().len(),
                V::Array(arr) => arr.borrow().len(),
                _ => 0,
            })
            .sum();

        match first {
            V::Array(_) => {
                let mut merged = Vec::with_capacity(capacity);

                for item in arr.iter() {
                    match item {
                        V::Array(inner) => {
                            let inner = inner.borrow();
                            merged.extend(inner.iter().cloned());
                        }
                        V::Null => {}
                        _ => return Err(anyhow!("Expected array of arrays")),
                    }
                }

                Ok(V::from_array(merged))
            }
            V::Object(_) => {
                let mut merged = zen_types::variable::VariableMap::with_capacity(capacity);
                for item in arr.iter() {
                    match item {
                        V::Object(obj) => {
                            let obj = obj.borrow();
                            for (key, value) in obj.iter() {
                                merged.insert(key.clone(), value.clone());
                            }
                        }
                        V::Null => {}
                        _ => return Err(anyhow!("Expected array of objects")),
                    }
                }

                Ok(V::from_object(merged))
            }
            other => Err(anyhow!(
                "merge expects an array of arrays or objects, got {}",
                other.type_name()
            )),
        }
    }

    pub fn merge_deep(args: Arguments) -> anyhow::Result<V> {
        let a = args.array(0)?;
        let arr = a.borrow();

        let mut result = V::empty_object();
        for item in arr.iter() {
            match item {
                V::Object(_) => {
                    result = deep_merge_variables(&result, item);
                }
                V::Null => {}
                _ => return Err(anyhow!("Expected array of objects")),
            }
        }

        Ok(result)
    }

    fn deep_merge_variables(base: &V, patch: &V) -> V {
        match (base, patch) {
            (V::Object(a), V::Object(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                let mut merged = zen_types::variable::VariableMap::with_capacity(a.len() + b.len());

                for (key, value) in a.iter() {
                    merged.insert(key.clone(), value.clone());
                }

                for (key, value) in b.iter() {
                    let entry = merged
                        .get(key)
                        .map(|existing| deep_merge_variables(existing, value))
                        .unwrap_or_else(|| value.clone());
                    merged.insert(key.clone(), entry);
                }

                V::from_object(merged)
            }
            (V::Array(a), V::Array(b)) => {
                let a = a.borrow();
                let b = b.borrow();
                let mut merged = Vec::with_capacity(a.len() + b.len());
                merged.extend(a.iter().cloned());
                merged.extend(b.iter().cloned());
                V::from_array(merged)
            }
            (_, patch) => patch.clone(),
        }
    }

    pub fn abs(args: Arguments) -> anyhow::Result<V> {
        let a = args.number(0)?;
        Ok(V::Number(a.abs()))
    }

    pub fn ceil(args: Arguments) -> anyhow::Result<V> {
        let a = args.number(0)?;
        Ok(V::Number(a.ceil()))
    }

    pub fn floor(args: Arguments) -> anyhow::Result<V> {
        let a = args.number(0)?;
        Ok(V::Number(a.floor()))
    }

    pub fn round(args: Arguments) -> anyhow::Result<V> {
        let a = args.number(0)?;
        let dp = args
            .onumber(1)?
            .map(|v| v.to_u32().context("Invalid number of decimal places"))
            .transpose()?
            .unwrap_or(0);

        Ok(V::Number(a.round_dp_with_strategy(
            dp,
            RoundingStrategy::MidpointAwayFromZero,
        )))
    }

    pub fn trunc(args: Arguments) -> anyhow::Result<V> {
        let a = args.number(0)?;
        let dp = args
            .onumber(1)?
            .map(|v| v.to_u32().context("Invalid number of decimal places"))
            .transpose()?
            .unwrap_or(0);

        Ok(V::Number(a.trunc_with_scale(dp)))
    }

    pub fn rand(args: Arguments) -> anyhow::Result<V> {
        let a = args.number(0)?;
        let upper_range = a.round().to_i64().context("Invalid upper range")?;

        let random_number = crate::context::Draw::fresh()
            .int(upper_range)
            .context("Invalid upper range")?;
        Ok(V::Number(Decimal::from(random_number)))
    }

    pub fn min(args: Arguments) -> anyhow::Result<V> {
        let a = __internal_number_or_date_array(&args, 0)?;

        match a {
            Either::Left(arr) => {
                let max = arr.into_iter().min().context("Empty array")?;
                Ok(V::Number(Decimal::from(max)))
            }
            Either::Right(arr) => {
                let max = arr.into_iter().min().context("Empty array")?;
                Ok(V::Dynamic(Rc::new(max)))
            }
        }
    }

    pub fn max(args: Arguments) -> anyhow::Result<V> {
        let a = __internal_number_or_date_array(&args, 0)?;

        match a {
            Either::Left(arr) => {
                let max = arr.into_iter().max().context("Empty array")?;
                Ok(V::Number(Decimal::from(max)))
            }
            Either::Right(arr) => {
                let max = arr.into_iter().max().context("Empty array")?;
                Ok(V::Dynamic(Rc::new(max)))
            }
        }
    }

    pub fn avg(args: Arguments) -> anyhow::Result<V> {
        let a = __internal_number_array(&args, 0)?;
        let sum = a
            .iter()
            .try_fold(Decimal::ZERO, |acc, x| acc.checked_add(*x))
            .context("Number overflow")?;

        Ok(V::Number(Decimal::from(
            sum.checked_div(Decimal::from(a.len()))
                .context("Empty array")?,
        )))
    }

    pub fn sum(args: Arguments) -> anyhow::Result<V> {
        let a = __internal_number_array(&args, 0)?;
        let sum = a
            .iter()
            .try_fold(Decimal::ZERO, |acc, v| acc.checked_add(*v))
            .context("Number overflow")?;

        Ok(V::Number(Decimal::from(sum)))
    }

    pub fn median(args: Arguments) -> anyhow::Result<V> {
        let mut a = __internal_number_array(&args, 0)?;
        a.sort();

        let center = a.len() / 2;
        if a.len() % 2 == 1 {
            let center_num = a.get(center).context("Index out of bounds")?;
            Ok(V::Number(*center_num))
        } else {
            let center_left = center
                .checked_sub(1)
                .and_then(|i| a.get(i))
                .context("Index out of bounds")?;
            let center_right = a.get(center).context("Index out of bounds")?;

            let median = center_left
                .checked_add(*center_right)
                .context("Number overflow")?
                / dec!(2);
            Ok(V::Number(median))
        }
    }

    pub fn mode(args: Arguments) -> anyhow::Result<V> {
        let a = __internal_number_array(&args, 0)?;
        let mut counts = BTreeMap::new();
        for num in a {
            *counts.entry(num).or_insert(0) += 1;
        }

        let most_common = counts
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map(|(num, _)| num)
            .context("Empty array")?;

        Ok(V::Number(most_common))
    }

    fn __internal_list(args: &Arguments, pos: usize) -> anyhow::Result<Vec<V>> {
        let a = args.array(pos)?;
        let items = a.borrow();
        Ok(items.clone())
    }

    fn __internal_aggregate(
        args: Arguments,
        f: impl FnOnce(&[V]) -> anyhow::Result<V>,
    ) -> anyhow::Result<V> {
        let a = args.array(0)?;
        let items = a.borrow();
        f(&items)
    }

    pub fn stddev(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::stddev(items, true))
    }

    pub fn variance(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::variance(items, true))
    }

    pub fn stddev_pop(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::stddev(items, false))
    }

    pub fn variance_pop(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::variance(items, false))
    }

    pub fn skew(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, aggregate::skew)
    }

    pub fn kurtosis(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, aggregate::kurtosis)
    }

    pub fn percentile(args: Arguments) -> anyhow::Result<V> {
        let q = args.number(1)?;
        __internal_aggregate(args, |items| aggregate::percentile(items, q))
    }

    pub fn percentile_approx(args: Arguments) -> anyhow::Result<V> {
        let q = args.number(1)?;
        let items = __internal_list(&args, 0)?;
        aggregate::percentile_approx(&items, q)
    }

    pub fn aggregate_sum(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, aggregate::sum)
    }

    pub fn aggregate_avg(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, aggregate::avg)
    }

    pub fn aggregate_min(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::extreme(items, false))
    }

    pub fn aggregate_max(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::extreme(items, true))
    }

    pub fn aggregate_median(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, aggregate::median)
    }

    pub fn aggregate_mode(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, aggregate::mode)
    }

    pub fn count_distinct(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| Ok(aggregate::count_distinct(items)))
    }

    pub fn count_distinct_approx(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| Ok(aggregate::count_distinct_approx(items)))
    }

    pub fn distinct(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| Ok(aggregate::distinct(items)))
    }

    pub fn top_k(args: Arguments) -> anyhow::Result<V> {
        let k = aggregate::count(args.var(1)?, 1)?;
        __internal_aggregate(args, |items| Ok(aggregate::top_k(items, k)))
    }

    pub fn first(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| Ok(aggregate::first(items)))
    }

    pub fn last(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| Ok(aggregate::last(items)))
    }

    pub fn arg_max(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::arg_extreme(items, true))
    }

    pub fn arg_min(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::arg_extreme(items, false))
    }

    pub fn take(args: Arguments) -> anyhow::Result<V> {
        let n = aggregate::count(args.var(1)?, 1)?;
        __internal_aggregate(args, |items| {
            Ok(V::from_array(items.iter().take(n).cloned().collect()))
        })
    }

    pub fn take_last(args: Arguments) -> anyhow::Result<V> {
        let n = aggregate::count(args.var(1)?, 1)?;
        __internal_aggregate(args, |items| {
            Ok(V::from_array(
                items[items.len().saturating_sub(n)..].to_vec(),
            ))
        })
    }

    pub fn find_index(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::find_index(items, false))
    }

    pub fn find_last_index(args: Arguments) -> anyhow::Result<V> {
        __internal_aggregate(args, |items| aggregate::find_index(items, true))
    }

    fn __internal_direction(args: &Arguments, pos: usize) -> anyhow::Result<bool> {
        match args.ostr(pos)? {
            None | Some("asc") => Ok(false),
            Some("desc") => Ok(true),
            Some(other) => Err(anyhow!("Expected 'asc' or 'desc', got '{other}'")),
        }
    }

    pub fn sort(args: Arguments) -> anyhow::Result<V> {
        let descending = __internal_direction(&args, 1)?;
        __internal_aggregate(args, |items| aggregate::sort(items, descending))
    }

    pub fn sort_by(args: Arguments) -> anyhow::Result<V> {
        let descending = __internal_direction(&args, 1)?;
        __internal_aggregate(args, |items| aggregate::sort_by(items, descending))
    }

    pub fn is_empty(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let empty = match a {
            V::Null => true,
            V::String(s) => s.is_empty(),
            V::Array(items) => items.borrow().is_empty(),
            V::Object(map) => map.borrow().is_empty(),
            other => {
                return Err(anyhow!(
                    "Cannot tell whether a {} is empty",
                    other.type_name()
                ))
            }
        };
        Ok(V::Bool(empty))
    }

    pub fn to_type(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        Ok(V::String(a.type_name().into()))
    }

    pub fn to_bool(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let a = match a.dynamic::<VmDate>().and_then(VmDate::source) {
            Some(text) => V::String(text.into()),
            None => a.clone(),
        };
        let val = match &a {
            V::Null => false,
            V::Bool(v) => *v,
            V::Number(n) => !n.is_zero(),
            V::Array(_) | V::Object(_) | V::Dynamic(_) => true,
            V::String(s) => match s.as_str().trim() {
                "true" => true,
                "false" => false,
                _ => s.is_empty(),
            },
        };

        Ok(V::Bool(val))
    }

    pub fn to_string(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let val = match a {
            V::Null => "null".into(),
            V::Bool(v) => v.to_string().into(),
            V::Number(n) => n.to_string().into(),
            V::String(s) => s.clone(),
            V::Dynamic(d) if d.as_date().is_some() => d.to_string().into(),
            _ => return Err(anyhow!("Cannot convert type {} to string", a.type_name())),
        };

        Ok(V::String(val))
    }

    pub fn to_number(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let val = match a {
            V::Number(n) => *n,
            V::String(str) => {
                let s = str.as_str().trim();
                Decimal::from_str_exact(s)
                    .or_else(|_| Decimal::from_scientific(s))
                    .context("Invalid number")?
            }
            V::Bool(b) => match *b {
                true => Decimal::ONE,
                false => Decimal::ZERO,
            },
            _ => return Err(anyhow!("Cannot convert type {} to number", a.type_name())),
        };

        Ok(V::Number(val))
    }

    pub fn is_numeric(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let is_ok = match a {
            V::Number(_) => true,
            V::String(str) => {
                let s = str.as_str().trim();
                Decimal::from_str_exact(s)
                    .or_else(|_| Decimal::from_scientific(s))
                    .is_ok()
            }
            _ => false,
        };

        Ok(V::Bool(is_ok))
    }

    pub fn len(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let len = match (a, a.as_str()) {
            (V::Array(s), _) => s.borrow().len(),
            (_, Some(text)) => text.len(),
            _ => {
                return Err(anyhow!("Cannot determine len of type {}", a.type_name()));
            }
        };

        Ok(V::Number(len.into()))
    }

    pub fn contains(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let b = args.var(1)?;

        let val = match (a, b, a.as_str().zip(b.as_str())) {
            (V::Array(a), _, _) => {
                let arr = a.borrow();

                arr.iter().any(|a| match (a, b) {
                    (V::Number(a), V::Number(b)) => a == b,
                    (V::String(a), V::String(b)) => a == b,
                    (V::Bool(a), V::Bool(b)) => a == b,
                    (V::Null, V::Null) => true,
                    (V::Dynamic(d), other) | (other, V::Dynamic(d)) => {
                        d.as_date().is_some_and(|d| d.matches(other))
                    }
                    _ => false,
                })
            }
            (_, _, Some((a, b))) => a.contains(b),
            _ => {
                return Err(anyhow!(
                    "Cannot determine contains for type {} and {}",
                    a.type_name(),
                    b.type_name()
                ));
            }
        };

        Ok(V::Bool(val))
    }

    pub fn fuzzy_match(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let b = args.str(1)?;

        let val = match (a, a.as_str()) {
            (_, Some(a)) => {
                let sim = strsim::normalized_damerau_levenshtein(a, b.as_ref());
                // This is okay, as NDL will return [0, 1]
                V::Number(Decimal::from_f64(sim).unwrap_or(dec!(0)))
            }
            (V::Array(_a), _) => {
                let a = _a.borrow();
                let mut sims = Vec::with_capacity(a.len());
                for v in a.iter() {
                    let s = v.as_str().context("Expected string array")?;

                    let sim = Decimal::from_f64(strsim::normalized_damerau_levenshtein(
                        s.as_ref(),
                        b.as_ref(),
                    ))
                    .unwrap_or(dec!(0));
                    sims.push(V::Number(sim));
                }

                V::from_array(sims)
            }
            _ => return Err(anyhow!("Fuzzy match not available for type")),
        };

        Ok(val)
    }

    pub fn keys(args: Arguments) -> anyhow::Result<V> {
        let a = args.var(0)?;
        let var = match a {
            V::Array(a) => {
                let arr = a.borrow();
                let indices = arr
                    .iter()
                    .enumerate()
                    .map(|(index, _)| V::Number(index.into()))
                    .collect();

                V::from_array(indices)
            }
            V::Object(a) => {
                let obj = a.borrow();
                let keys = obj
                    .iter()
                    .map(|(key, _)| V::String((key.as_str()).into()))
                    .collect();

                V::from_array(keys)
            }
            _ => {
                return Err(anyhow!("Cannot determine keys of type {}", a.type_name()));
            }
        };

        Ok(var)
    }

    pub fn values(args: Arguments) -> anyhow::Result<V> {
        let a = args.object(0)?;
        let obj = a.borrow();
        let values: Vec<_> = obj.values().cloned().collect();

        Ok(V::from_array(values))
    }

    pub fn date(args: Arguments) -> anyhow::Result<V> {
        let provided = args.ovar(0);
        let tz = args
            .ostr(1)?
            .map(|v| Tz::from_str(v).context("Invalid timezone"))
            .transpose()?;

        let date_time = match provided {
            Some(v) => VmDate::new(v.clone(), tz),
            None => VmDate::now(),
        };

        Ok(V::Dynamic(Rc::new(date_time)))
    }
}

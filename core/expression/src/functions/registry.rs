use crate::functions::defs::{FunctionDefinition, FunctionSignature, StaticFunction};
use crate::functions::{ClosureFunction, DeprecatedFunction, FunctionKind, InternalFunction};
use nohash_hasher::{BuildNoHashHasher, IsEnabled};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use strum::IntoEnumIterator;

impl IsEnabled for InternalFunction {}
impl IsEnabled for DeprecatedFunction {}

pub struct FunctionRegistry {
    internal_functions:
        HashMap<InternalFunction, Rc<dyn FunctionDefinition>, BuildNoHashHasher<InternalFunction>>,
    deprecated_functions: HashMap<
        DeprecatedFunction,
        Rc<dyn FunctionDefinition>,
        BuildNoHashHasher<DeprecatedFunction>,
    >,
    closure_aggregates: HashMap<ClosureFunction, Rc<dyn FunctionDefinition>>,
}

impl FunctionRegistry {
    thread_local!(
        static INSTANCE: RefCell<FunctionRegistry> = RefCell::new(FunctionRegistry::new_internal())
    );

    pub fn get_definition(kind: &FunctionKind) -> Option<Rc<dyn FunctionDefinition>> {
        match kind {
            FunctionKind::Internal(internal) => {
                Self::INSTANCE.with_borrow(|i| i.internal_functions.get(&internal).cloned())
            }
            FunctionKind::Deprecated(deprecated) => {
                Self::INSTANCE.with_borrow(|i| i.deprecated_functions.get(&deprecated).cloned())
            }
            FunctionKind::Closure(closure) => {
                Self::INSTANCE.with_borrow(|i| i.closure_aggregates.get(closure).cloned())
            }
        }
    }

    fn new_internal() -> Self {
        let internal_functions = InternalFunction::iter()
            .map(|i| (i.clone(), (&i).into()))
            .collect();

        let deprecated_functions = DeprecatedFunction::iter()
            .map(|i| (i.clone(), (&i).into()))
            .collect();

        let closure_aggregates = ClosureFunction::iter()
            .filter_map(|c| Some((c, aggregate::definition(c)?)))
            .collect();

        Self {
            internal_functions,
            deprecated_functions,
            closure_aggregates,
        }
    }
}

/// What the callback forms of the aggregates do with the values they
/// collected (projected, filtered, nulls skipped). Empty input: `sum`,
/// `countDistinct` and `countDistinctApprox` are `0`, `unique` is `[]`,
/// the others are `null`. `argMax` and `argMin` collect `[value, by]` pairs.
mod aggregate {
    use super::*;
    use crate::functions::arguments::Arguments;
    use crate::functions::internal::imp;
    use crate::functions::moments::{self, power_sums};
    use crate::functions::sketch::Hll;
    use crate::variable::VariableType as VT;
    use crate::vm::date::DynamicVariableExt;
    use crate::vm::VmDate;
    use crate::Variable;
    use anyhow::Context;
    use rust_decimal::Decimal;
    use std::collections::HashSet;

    type Implementation = fn(Arguments) -> anyhow::Result<Variable>;

    /// `None` for the callbacks that are no aggregate (`map`, `filter`...).
    pub(super) fn definition(kind: ClosureFunction) -> Option<Rc<dyn FunctionDefinition>> {
        let (return_type, implementation): (VT, Implementation) = match kind {
            ClosureFunction::Sum => (VT::Number, imp::sum),
            ClosureFunction::Avg => (VT::Number, avg),
            ClosureFunction::Min => (VT::Any, min),
            ClosureFunction::Max => (VT::Any, max),
            ClosureFunction::Median => (VT::Number, median),
            ClosureFunction::Mode => (VT::Any, mode),
            ClosureFunction::Stddev => (VT::Number, imp::stddev),
            ClosureFunction::Variance => (VT::Number, imp::variance),
            ClosureFunction::TopK
            | ClosureFunction::LastN
            | ClosureFunction::Percentile
            | ClosureFunction::PercentileApprox => {
                let (return_type, implementation): (VT, Implementation) = match kind {
                    ClosureFunction::TopK => (VT::Any.array(), imp::top_k),
                    ClosureFunction::LastN => (VT::Any.array(), imp::last_n),
                    ClosureFunction::PercentileApprox => (VT::Number, imp::percentile_approx),
                    _ => (VT::Number, imp::percentile),
                };
                return Some(Rc::new(StaticFunction {
                    signature: FunctionSignature {
                        parameters: vec![VT::Any.array(), VT::Number],
                        return_type,
                    },
                    implementation: Rc::new(implementation),
                }));
            }
            ClosureFunction::CountDistinct => (VT::Number, count_distinct),
            ClosureFunction::Unique => (VT::Any.array(), unique),
            ClosureFunction::First => (VT::Any, first),
            ClosureFunction::Last => (VT::Any, last),
            ClosureFunction::ArgMax => (VT::Any, arg_max),
            ClosureFunction::ArgMin => (VT::Any, arg_min),
            ClosureFunction::Skew => (VT::Number, skew),
            ClosureFunction::Kurtosis => (VT::Number, kurtosis),
            ClosureFunction::CountDistinctApprox => (VT::Number, count_distinct_approx),
            ClosureFunction::All
            | ClosureFunction::None
            | ClosureFunction::Some
            | ClosureFunction::One
            | ClosureFunction::Filter
            | ClosureFunction::Map
            | ClosureFunction::FlatMap
            | ClosureFunction::Count => return None,
        };

        Some(Rc::new(StaticFunction {
            signature: FunctionSignature::single(VT::Any.array(), return_type),
            implementation: Rc::new(implementation),
        }))
    }

    fn null_if_empty(args: Arguments, f: Implementation) -> anyhow::Result<Variable> {
        if args.array(0)?.borrow().is_empty() {
            return Ok(Variable::Null);
        }

        f(args)
    }

    fn avg(args: Arguments) -> anyhow::Result<Variable> {
        null_if_empty(args, imp::avg)
    }

    fn min(args: Arguments) -> anyhow::Result<Variable> {
        null_if_empty(args, imp::min)
    }

    fn max(args: Arguments) -> anyhow::Result<Variable> {
        null_if_empty(args, imp::max)
    }

    fn median(args: Arguments) -> anyhow::Result<Variable> {
        null_if_empty(args, imp::median)
    }

    /// The most common value (numbers or strings); ties go to the largest.
    fn mode(args: Arguments) -> anyhow::Result<Variable> {
        null_if_empty(args, imp::mode)
    }

    /// What `countDistinct` and `unique` compare: scalars as they are
    /// (numbers normalized), objects and arrays by a canonical text with
    /// object keys sorted, so `{a: 1, b: 2}` and `{b: 2, a: 1}` are one.
    #[derive(PartialEq, Eq, Hash)]
    enum Key<'a> {
        Bool(bool),
        Number(rust_decimal::Decimal),
        String(&'a str),
        Canonical(String),
    }

    fn key(value: &Variable) -> Key<'_> {
        match value {
            Variable::Bool(b) => Key::Bool(*b),
            Variable::Number(n) => Key::Number(n.normalize()),
            Variable::String(s) => Key::String(s.as_str()),
            other => {
                let mut text = String::new();
                canonical(other, &mut text);
                Key::Canonical(text)
            }
        }
    }

    fn canonical(value: &Variable, out: &mut String) {
        use std::fmt::Write;
        match value {
            Variable::Null => out.push_str("null"),
            Variable::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Variable::Number(n) => {
                let _ = write!(out, "{}", n.normalize());
            }
            Variable::String(s) => {
                let _ = write!(out, "{:?}", s.as_str());
            }
            Variable::Array(items) => {
                out.push('[');
                for (i, item) in items.borrow().iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    canonical(item, out);
                }
                out.push(']');
            }
            Variable::Object(fields) => {
                let fields = fields.borrow();
                let mut entries: Vec<_> = fields.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                out.push('{');
                for (i, (name, item)) in entries.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    let _ = write!(out, "{:?}:", name.as_str());
                    canonical(item, out);
                }
                out.push('}');
            }
            Variable::Dynamic(dynamic) => {
                let _ = write!(out, "{}:{dynamic}", dynamic.type_name());
            }
        }
    }

    fn distinct(args: &Arguments) -> anyhow::Result<Vec<Variable>> {
        let array = args.array(0)?;
        let items = array.borrow();
        let mut seen = HashSet::with_capacity(items.len());
        Ok(items
            .iter()
            .filter(|item| !matches!(item, Variable::Null) && seen.insert(key(item)))
            .cloned()
            .collect())
    }

    fn count_distinct(args: Arguments) -> anyhow::Result<Variable> {
        Ok(Variable::Number(distinct(&args)?.len().into()))
    }

    fn unique(args: Arguments) -> anyhow::Result<Variable> {
        Ok(Variable::from_array(distinct(&args)?))
    }

    fn first(args: Arguments) -> anyhow::Result<Variable> {
        Ok(args
            .array(0)?
            .borrow()
            .first()
            .cloned()
            .unwrap_or(Variable::Null))
    }

    fn last(args: Arguments) -> anyhow::Result<Variable> {
        Ok(args
            .array(0)?
            .borrow()
            .last()
            .cloned()
            .unwrap_or(Variable::Null))
    }

    fn numbers(args: &Arguments) -> anyhow::Result<Vec<Decimal>> {
        let array = args.array(0)?;
        let items = array.borrow();
        items
            .iter()
            .map(|item| item.as_number())
            .collect::<Option<Vec<_>>>()
            .context("Expected a number array")
    }

    /// What `argMax` and `argMin` rank by: numbers, or dates (as `min` and
    /// `max` compare them).
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum Rank {
        Number(Decimal),
        Date(VmDate),
    }

    fn rank(value: &Variable) -> anyhow::Result<Rank> {
        match value {
            Variable::Number(n) => Ok(Rank::Number(*n)),
            Variable::Dynamic(d) => match d.as_date() {
                Some(date) => Ok(Rank::Date(date.clone())),
                None => anyhow::bail!("Expected numbers or dates to rank by"),
            },
            _ => anyhow::bail!("Expected numbers or dates to rank by"),
        }
    }

    /// The value paired with the greatest (`greatest`) or smallest rank;
    /// ties go to the last pair. `null` without pairs.
    fn arg_extreme(args: Arguments, greatest: bool) -> anyhow::Result<Variable> {
        let array = args.array(0)?;
        let items = array.borrow();
        let mut best: Option<(Rank, Variable)> = None;
        for item in items.iter() {
            let Variable::Array(pair) = item else {
                anyhow::bail!("Expected [value, by] pairs");
            };
            let pair = pair.borrow();
            let [value, by] = pair.as_slice() else {
                anyhow::bail!("Expected [value, by] pairs");
            };
            let by = rank(by)?;
            let replaces = match &best {
                None => true,
                Some((current, _)) => {
                    if std::mem::discriminant(current) != std::mem::discriminant(&by) {
                        anyhow::bail!("Cannot rank numbers and dates together");
                    }
                    if greatest {
                        by >= *current
                    } else {
                        by <= *current
                    }
                }
            };
            if replaces {
                best = Some((by, value.clone()));
            }
        }
        Ok(best.map_or(Variable::Null, |(_, value)| value))
    }

    fn arg_max(args: Arguments) -> anyhow::Result<Variable> {
        arg_extreme(args, true)
    }

    fn arg_min(args: Arguments) -> anyhow::Result<Variable> {
        arg_extreme(args, false)
    }

    /// Population skewness and excess kurtosis from exact power sums;
    /// `null` below two values, for equal values or on overflow.
    fn shape(
        args: Arguments,
        f: fn(i64, [Decimal; 4]) -> Option<Decimal>,
    ) -> anyhow::Result<Variable> {
        let values = numbers(&args)?;
        Ok(power_sums(values)
            .and_then(|(n, s)| f(n, s))
            .map_or(Variable::Null, Variable::Number))
    }

    fn skew(args: Arguments) -> anyhow::Result<Variable> {
        shape(args, moments::skew)
    }

    fn kurtosis(args: Arguments) -> anyhow::Result<Variable> {
        shape(args, moments::kurtosis)
    }

    /// HyperLogLog estimate of the distinct scalars (see [`Hll`]); arrays
    /// and objects have no stable hash and are skipped.
    fn count_distinct_approx(args: Arguments) -> anyhow::Result<Variable> {
        let array = args.array(0)?;
        let mut hll = Hll::new();
        for item in array.borrow().iter() {
            hll.insert(item);
        }
        Ok(Variable::Number(hll.estimate()))
    }
}

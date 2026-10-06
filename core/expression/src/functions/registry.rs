use crate::functions::defs::{FunctionDefinition, FunctionSignature, StaticFunction};
use crate::functions::{ClosureFunction, DeprecatedFunction, FunctionKind, InternalFunction};
use nohash_hasher::{BuildNoHashHasher, IsEnabled};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock, RwLock};
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
    host_functions: HashMap<Arc<str>, Rc<dyn FunctionDefinition>>,
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
            FunctionKind::Host(name) => {
                if let Some(found) =
                    Self::INSTANCE.with_borrow(|i| i.host_functions.get(name).cloned())
                {
                    return Some(found);
                }

                let host = host_registry().read().ok()?.get(name).cloned()?;
                let definition = (host.definition)();
                Self::INSTANCE
                    .with_borrow_mut(|i| i.host_functions.insert(name.clone(), definition.clone()));
                Some(definition)
            }
        }
    }

    /// The registered host functions: name and description, sorted by name.
    pub fn host_functions() -> Vec<(Arc<str>, String)> {
        let Ok(registry) = host_registry().read() else {
            return Vec::new();
        };

        let mut list: Vec<(Arc<str>, String)> = registry
            .iter()
            .map(|(name, host)| (name.clone(), host.description.clone()))
            .collect();
        list.sort();
        list
    }

    pub(crate) fn host_description(name: &str) -> Option<String> {
        host_registry()
            .read()
            .ok()?
            .get(name)
            .map(|host| host.description.clone())
    }

    fn new_internal() -> Self {
        let internal_functions = InternalFunction::iter()
            .map(|i| (i.clone(), (&i).into()))
            .collect();

        let deprecated_functions = DeprecatedFunction::iter()
            .map(|i| (i.clone(), (&i).into()))
            .collect();

        let closure_aggregates = ClosureFunction::iter()
            .filter(|c| c.is_aggregate())
            .map(|c| (c, aggregate::definition(c)))
            .collect();

        Self {
            internal_functions,
            deprecated_functions,
            closure_aggregates,
            host_functions: HashMap::new(),
        }
    }
}

/// A function the host application adds to the language: the parser, the
/// type checker, intellisense and the VM accept it like a built-in.
///
/// `definition` builds its [`FunctionDefinition`] (signatures, and the
/// implementation the VM calls); it runs once per thread that uses it. A
/// host that only parses and type-checks (and evaluates elsewhere) can give
/// an implementation that returns an error.
#[derive(Clone)]
pub struct HostFunction {
    pub name: Arc<str>,
    pub description: String,
    pub definition: Arc<dyn Fn() -> Rc<dyn FunctionDefinition> + Send + Sync>,
}

fn host_registry() -> &'static RwLock<HashMap<Arc<str>, HostFunction>> {
    static REGISTRY: OnceLock<RwLock<HashMap<Arc<str>, HostFunction>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Registers a host function for the whole process. Built-in names cannot
/// be taken; registering a name again replaces it on threads that have not
/// used it yet.
pub fn register_host_function(function: HostFunction) -> anyhow::Result<()> {
    let name = function.name.as_ref();
    if InternalFunction::try_from(name).is_ok()
        || DeprecatedFunction::try_from(name).is_ok()
        || ClosureFunction::try_from(name).is_ok()
    {
        anyhow::bail!("`{name}` is a built-in function");
    }

    let mut registry = host_registry()
        .write()
        .map_err(|_| anyhow::anyhow!("host function registry is poisoned"))?;
    registry.insert(function.name.clone(), function);
    Ok(())
}

pub(crate) fn host_function_name(name: &str) -> Option<Arc<str>> {
    let registry = host_registry().read().ok()?;
    registry.get_key_value(name).map(|(key, _)| key.clone())
}

/// What the callback forms of the aggregates do with the values they
/// collected (projected, filtered, nulls skipped). Empty input: `sum` and
/// `countDistinct` are `0`, `unique` is `[]`, the others are `null`.
mod aggregate {
    use super::*;
    use crate::functions::arguments::Arguments;
    use crate::functions::internal::imp;
    use crate::variable::VariableType as VT;
    use crate::Variable;
    use std::collections::HashSet;

    type Implementation = fn(Arguments) -> anyhow::Result<Variable>;

    pub(super) fn definition(kind: ClosureFunction) -> Rc<dyn FunctionDefinition> {
        let (return_type, implementation): (VT, Implementation) = match kind {
            ClosureFunction::Sum => (VT::Number, imp::sum),
            ClosureFunction::Avg => (VT::Number, avg),
            ClosureFunction::Min => (VT::Any, min),
            ClosureFunction::Max => (VT::Any, max),
            ClosureFunction::Median => (VT::Number, median),
            ClosureFunction::Mode => (VT::Any, mode),
            ClosureFunction::Stddev => (VT::Number, imp::stddev),
            ClosureFunction::Variance => (VT::Number, imp::variance),
            ClosureFunction::TopK | ClosureFunction::LastN | ClosureFunction::Percentile => {
                let (return_type, implementation): (VT, Implementation) = match kind {
                    ClosureFunction::TopK => (VT::Any.array(), imp::top_k),
                    ClosureFunction::LastN => (VT::Any.array(), imp::last_n),
                    _ => (VT::Number, imp::percentile),
                };
                return Rc::new(StaticFunction {
                    signature: FunctionSignature {
                        parameters: vec![VT::Any.array(), VT::Number],
                        return_type,
                    },
                    implementation: Rc::new(implementation),
                });
            }
            ClosureFunction::CountDistinct => (VT::Number, count_distinct),
            ClosureFunction::Unique => (VT::Any.array(), unique),
            ClosureFunction::First => (VT::Any, first),
            ClosureFunction::Last => (VT::Any, last),
            _ => (VT::Any, unsupported),
        };

        Rc::new(StaticFunction {
            signature: FunctionSignature::single(VT::Any.array(), return_type),
            implementation: Rc::new(implementation),
        })
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
        let array = args.array(0)?;
        let items = array.borrow();
        if items.is_empty() {
            return Ok(Variable::Null);
        }
        if items.iter().all(|item| matches!(item, Variable::Number(_))) {
            drop(items);
            return imp::mode(args);
        }
        let mut counts = std::collections::BTreeMap::new();
        for item in items.iter() {
            let Variable::String(s) = item else {
                anyhow::bail!("Expected an array of numbers or strings");
            };
            *counts.entry(s.clone()).or_insert(0usize) += 1;
        }
        Ok(counts
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map_or(Variable::Null, |(s, _)| Variable::String(s)))
    }

    fn key(value: &Variable) -> String {
        match value {
            Variable::Number(n) => format!("number:{}", n.normalize()),
            other => format!("{}:{other}", other.type_name()),
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

    fn unsupported(_: Arguments) -> anyhow::Result<Variable> {
        anyhow::bail!("not an aggregate")
    }
}

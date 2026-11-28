use crate::functions::defs::FunctionDefinition;
use crate::functions::{DeprecatedFunction, FunctionKind, InternalFunction};
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
    host_functions: HashMap<Arc<str>, Rc<dyn FunctionDefinition>>,
}

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

pub(crate) fn host_function_name(name: &str) -> Option<Arc<str>> {
    host_registry()
        .read()
        .ok()?
        .get_key_value(name)
        .map(|(key, _)| key.clone())
}

pub fn register_host_function(function: HostFunction) -> Result<(), String> {
    if FunctionKind::try_from(function.name.as_ref())
        .is_ok_and(|kind| !matches!(kind, FunctionKind::Host(_)))
    {
        return Err(format!("{} is a built-in function", function.name));
    }
    let valid = function
        .name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && function
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(format!("{} is not a valid function name", function.name));
    }
    let mut registry = host_registry()
        .write()
        .map_err(|_| "the host function registry is poisoned".to_string())?;
    registry.insert(function.name.clone(), function);
    Ok(())
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
            FunctionKind::Closure(_) => None,
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

        Self {
            internal_functions,
            deprecated_functions,
            host_functions: HashMap::new(),
        }
    }
}

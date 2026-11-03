use crate::compiled::host::definition::{NodeDataType, TraceDataType};
use crate::compiled::host::extensions::NodeHandlerExtensions;
use crate::compiled::host::function::v2::function::Function;
use crate::compiled::host::result::{NodeResponse, NodeResult};
use crate::compiled::host::validator_cache::Validators;
use crate::compiled::host::variable_json::{Guards, VariableNode};
use crate::compiled::host::NodeError;
use crate::ZEN_CONFIG;
use ahash::HashSet;
use jsonschema::ValidationError;
use serde::Serialize;
use serde_json::Value;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use thiserror::Error;
use zen_types::symbol::Symbol;
use zen_types::variable::{ToVariable, Variable};

#[derive(Clone)]
pub struct NodeContext<NodeData, TraceData>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub node: NodeData,
    pub input: Variable,
    pub nodes: Option<Variable>,
    pub trace: Option<RefCell<TraceData>>,
    pub extensions: NodeHandlerExtensions,
    pub iteration: u8,
    pub config: NodeContextConfig,
}

impl<NodeData, TraceData> NodeContext<NodeData, TraceData>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    pub fn input_with_nodes(&self) -> Variable {
        NodeContextBase::joined(&self.input, self.nodes.as_ref())
    }

    pub fn from_base(base: NodeContextBase, data: NodeData) -> Self {
        Self {
            id: base.id,
            name: base.name,
            input: base.input,
            nodes: base.nodes,
            extensions: base.extensions,
            iteration: base.iteration,
            trace: base.config.trace.then(Default::default),
            node: data,
            config: base.config,
        }
    }

    pub fn trace<Function>(&self, mutator: Function)
    where
        Function: FnOnce(&mut TraceData),
    {
        if let Some(trace) = &self.trace {
            mutator(&mut *trace.borrow_mut());
        }
    }

    pub fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: self.trace.as_ref().map(|v| (*v.borrow()).to_variable()),
        })
    }

    pub(crate) fn make_error<Error>(&self, error: Error) -> NodeError
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        NodeError::traced(
            self.id.clone(),
            self.trace.as_ref().map(|v| (*v.borrow()).to_variable()),
            error,
        )
    }

    pub(crate) async fn function_runtime(&self) -> Result<&Function, NodeError> {
        self.extensions.function_runtime().await.node_context(self)
    }

    pub fn validate(&self, schema: &Value, value: &Variable) -> Result<(), NodeError> {
        let validator = self
            .extensions
            .validator_cache()
            .get_or_insert(self.hash_node(), schema)
            .node_context(self)?;
        if validator.is_valid(VariableNode::new(value, &Guards::default())) {
            return Ok(());
        }

        {
            let guards = Guards::default();
            let mut first = None;
            let mut nullable = false;
            for error in validator.iter_errors(VariableNode::new(value, &guards)) {
                if Self::nullable(value, &error.instance_path().to_string()) {
                    nullable = true;
                    break;
                }
                first.get_or_insert(error);
            }
            if !nullable {
                return match first {
                    Some(error) => Err(ValidationErrorJson::from(error)).node_context(self),
                    None => Ok(()),
                };
            }
        }

        let current = value.deep_clone();
        let mut removed: HashSet<String> = HashSet::default();
        loop {
            let guards = Guards::default();
            let paths: Vec<String> = validator
                .iter_errors(VariableNode::new(&current, &guards))
                .map(|error| error.instance_path().to_string())
                .collect();
            drop(guards);
            if paths.is_empty() {
                return Ok(());
            }
            let before = removed.len();
            for path in &paths {
                Self::remove_nulls_at(&current, path, &mut removed);
            }
            if removed.len() == before {
                break;
            }
        }

        let guards = Guards::default();
        let mut errors: Vec<ValidationErrorJson> = validator
            .iter_errors(VariableNode::new(value, &guards))
            .map(ValidationErrorJson::from)
            .collect();
        let chosen = errors
            .iter()
            .position(|error| !removed.contains(&error.path))
            .unwrap_or(0);
        match chosen < errors.len() {
            true => Err(errors.swap_remove(chosen)).node_context(self),
            false => Ok(()),
        }
    }

    fn nullable(value: &Variable, pointer: &str) -> bool {
        let segments: Vec<String> = pointer
            .split('/')
            .skip(1)
            .map(|s| s.replace("~1", "/").replace("~0", "~"))
            .collect();
        match (Self::node_at(value, &segments), segments.split_last()) {
            (None, _) => false,
            (Some(Variable::Null), Some((_, parents))) => {
                matches!(Self::node_at(value, parents), Some(Variable::Object(_)))
            }
            (Some(target), _) => Self::holds_null(&target),
        }
    }

    fn holds_null(value: &Variable) -> bool {
        match value {
            Variable::Object(object) => object
                .borrow()
                .iter()
                .any(|(_, field)| matches!(field, Variable::Null) || Self::holds_null(field)),
            Variable::Array(array) => array.borrow().iter().any(Self::holds_null),
            _ => false,
        }
    }

    fn remove_nulls_at(value: &Variable, pointer: &str, removed: &mut HashSet<String>) {
        let segments: Vec<String> = pointer
            .split('/')
            .skip(1)
            .map(|s| s.replace("~1", "/").replace("~0", "~"))
            .collect();
        let Some(target) = Self::node_at(value, &segments) else {
            return;
        };
        match (&target, segments.split_last()) {
            (Variable::Null, Some((key, parents))) => {
                if let Some(Variable::Object(object)) = Self::node_at(value, parents) {
                    if object
                        .borrow_mut()
                        .remove(&Symbol::from(key.as_str()))
                        .is_some()
                    {
                        removed.insert(pointer.to_string());
                    }
                }
            }
            _ => Self::strip_nulls(&target, pointer, removed),
        }
    }

    fn node_at(value: &Variable, segments: &[String]) -> Option<Variable> {
        segments
            .iter()
            .try_fold(value.shallow_clone(), |current, segment| match &current {
                Variable::Object(o) => o
                    .borrow()
                    .get(&Symbol::from(segment.as_str()))
                    .map(Variable::shallow_clone),
                Variable::Array(a) => segment
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| a.borrow().get(i).map(Variable::shallow_clone)),
                _ => None,
            })
    }

    fn strip_nulls(value: &Variable, pointer: &str, removed: &mut HashSet<String>) {
        let children: Vec<(String, Variable)> = match value {
            Variable::Object(object) => {
                let entries: Vec<(Symbol, Variable)> = object
                    .borrow()
                    .iter()
                    .map(|(key, field)| (key.clone(), field.shallow_clone()))
                    .collect();
                let mut children = Vec::with_capacity(entries.len());
                for (key, field) in entries {
                    let child = format!(
                        "{pointer}/{}",
                        key.as_str().replace('~', "~0").replace('/', "~1")
                    );
                    match field {
                        Variable::Null => {
                            object.borrow_mut().remove(&key);
                            removed.insert(child);
                        }
                        field => children.push((child, field)),
                    }
                }
                children
            }
            Variable::Array(array) => array
                .borrow()
                .iter()
                .enumerate()
                .map(|(index, item)| (format!("{pointer}/{index}"), item.shallow_clone()))
                .collect(),
            _ => return,
        };
        for (child, field) in children {
            Self::strip_nulls(&field, &child, removed);
        }
    }

    fn hash_node(&self) -> u64 {
        Validators::key(&self.id, &self.name, self.config.validation_salt)
    }
}

pub trait NodeContextExt<T, Context>: Sized {
    type Error: Into<Box<dyn std::error::Error>>;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &Context,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>;

    fn node_context(self, ctx: &Context) -> Result<T, NodeError> {
        self.with_node_context(ctx, |e| e.into())
    }

    fn node_context_message(self, ctx: &Context, message: &str) -> Result<T, NodeError> {
        self.with_node_context(ctx, |err| format!("{}: {}", message, err.into()))
    }
}

impl<T, E, NodeData, TraceData> NodeContextExt<T, NodeContext<NodeData, TraceData>> for Result<T, E>
where
    E: Into<Box<dyn std::error::Error>>,
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    type Error = E;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContext<NodeData, TraceData>,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.map_err(|err| ctx.make_error(f(err)))
    }
}

impl<T, NodeData, TraceData> NodeContextExt<T, NodeContext<NodeData, TraceData>> for Option<T>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    type Error = &'static str;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContext<NodeData, TraceData>,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.ok_or_else(|| ctx.make_error(f("None")))
    }

    fn node_context_message(
        self,
        ctx: &NodeContext<NodeData, TraceData>,
        message: &str,
    ) -> Result<T, NodeError> {
        self.with_node_context(ctx, |_| message.to_string())
    }
}

#[derive(Clone)]
pub struct NodeContextBase {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub input: Variable,
    pub nodes: Option<Variable>,
    pub iteration: u8,
    pub extensions: NodeHandlerExtensions,
    pub config: NodeContextConfig,
    pub trace: Option<RefCell<Variable>>,
}

impl NodeContextBase {
    pub(crate) fn joined(input: &Variable, nodes: Option<&Variable>) -> Variable {
        let Some(nodes) = nodes else {
            return input.shallow_clone();
        };
        let Variable::Object(object) = input else {
            return input.shallow_clone();
        };

        let mut map = object.borrow().clone();
        map.insert(Variable::nodes_key(), nodes.clone());
        Variable::from_object(map)
    }

    pub fn error<Error>(&self, error: Error) -> NodeResult
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        Err(self.make_error(error))
    }

    pub fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: self.trace.as_ref().map(|v| v.borrow().to_variable()),
        })
    }

    fn make_error<Error>(&self, error: Error) -> NodeError
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        NodeError::traced(
            self.id.clone(),
            self.trace.as_ref().map(|t| t.borrow().to_variable()),
            error,
        )
    }
}

#[derive(Debug, Serialize, Error)]
#[serde(rename_all = "camelCase")]
struct ValidationErrorJson {
    path: String,
    message: String,
}

impl Display for ValidationErrorJson {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl<'a> From<ValidationError<'a>> for ValidationErrorJson {
    fn from(value: ValidationError<'a>) -> Self {
        ValidationErrorJson {
            path: value.instance_path().to_string(),
            message: format!("{}", value),
        }
    }
}

#[derive(Clone)]
pub struct NodeContextConfig {
    pub trace: bool,
    pub max_depth: u8,
    pub function_timeout_millis: u64,
    pub validation_salt: u64,
}

impl Default for NodeContextConfig {
    fn default() -> Self {
        Self {
            trace: false,
            function_timeout_millis: ZEN_CONFIG.function_timeout_millis.load(Ordering::Relaxed),
            max_depth: 5,
            validation_salt: 0,
        }
    }
}

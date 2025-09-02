use crate::nodes::definition::{NodeDataType, TraceDataType};
use crate::nodes::extensions::NodeHandlerExtensions;
use crate::nodes::function::v2::function::Function;
use crate::nodes::result::{NodeResponse, NodeResult};
use crate::nodes::NodeError;
use crate::ZEN_CONFIG;
use ahash::AHasher;
use jsonschema::ValidationError;
use serde::Serialize;
use serde_json::Value;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
use std::hash::Hasher;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use thiserror::Error;
use zen_types::variable::Variable;

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
    pub fn from_base(base: NodeContextBase, data: NodeData) -> Self {
        Self {
            id: base.id,
            name: base.name,
            input: base.input,
            extensions: base.extensions,
            iteration: base.iteration,
            trace: base.config.trace.then(|| Default::default()),
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

    pub fn error<Error>(&self, error: Error) -> NodeResult
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        Err(self.make_error(error))
    }

    pub fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: self.trace.as_ref().map(|v| (*v.borrow()).to_variable()),
        })
    }

    fn make_error<Error>(&self, error: Error) -> NodeError
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        NodeError {
            node_id: self.id.clone(),
            trace: self.trace.as_ref().map(|v| (*v.borrow()).to_variable()),
            source: error.into(),
        }
    }

    pub(crate) async fn function_runtime(&self) -> Result<&Function, NodeError> {
        self.extensions.function_runtime().await.node_context(self)
    }

    pub fn validate(&self, schema: &Value, value: &Value) -> Result<(), NodeError> {
        let validator_cache = self.extensions.validator_cache();
        let hash = self.hash_node();

        let validator = validator_cache
            .get_or_insert(hash, schema)
            .node_context(self)?;

        validator
            .validate(value)
            .map_err(|err| ValidationErrorJson::from(err))
            .node_context(self)?;

        Ok(())
    }

    fn hash_node(&self) -> u64 {
        let mut hasher = AHasher::default();
        hasher.write(self.id.as_bytes());
        hasher.write(self.name.as_bytes());
        hasher.finish()
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
}

#[derive(Clone)]
pub struct NodeContextBase {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub input: Variable,
    pub iteration: u8,
    pub extensions: NodeHandlerExtensions,
    pub config: NodeContextConfig,
}

impl NodeContextBase {
    pub fn error<Error>(&self, error: Error) -> NodeResult
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        Err(self.make_error(error))
    }

    pub fn success(&self, output: Variable) -> NodeResult {
        Ok(NodeResponse {
            output,
            trace_data: None,
        })
    }

    fn make_error<Error>(&self, error: Error) -> NodeError
    where
        Error: Into<Box<dyn std::error::Error>>,
    {
        NodeError {
            node_id: self.id.clone(),
            source: error.into(),
            trace: None,
        }
    }
}

impl<NodeData, TraceData> From<NodeContext<NodeData, TraceData>> for NodeContextBase
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    fn from(value: NodeContext<NodeData, TraceData>) -> Self {
        Self {
            id: value.id,
            name: value.name,
            input: value.input,
            extensions: value.extensions,
            iteration: value.iteration,
            config: value.config,
        }
    }
}

impl<T, E> NodeContextExt<T, NodeContextBase> for Result<T, E>
where
    E: Into<Box<dyn std::error::Error>>,
{
    type Error = E;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContextBase,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.map_err(|err| ctx.make_error(f(err)))
    }
}

impl<T> NodeContextExt<T, NodeContextBase> for Option<T> {
    type Error = &'static str;

    fn with_node_context<Function, NewError>(
        self,
        ctx: &NodeContextBase,
        f: Function,
    ) -> Result<T, NodeError>
    where
        Function: FnOnce(Self::Error) -> NewError,
        NewError: Into<Box<dyn std::error::Error>>,
    {
        self.ok_or_else(|| ctx.make_error(f("None")))
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
            path: value.instance_path.to_string(),
            message: format!("{}", value),
        }
    }
}

#[derive(Clone)]
pub struct NodeContextConfig {
    pub trace: bool,
    pub nodes_in_context: bool,
    pub max_depth: u8,
    pub function_timeout_millis: u64,
}

impl Default for NodeContextConfig {
    fn default() -> Self {
        Self {
            trace: false,
            nodes_in_context: ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed),
            function_timeout_millis: ZEN_CONFIG.function_timeout_millis.load(Ordering::Relaxed),
            max_depth: 5,
        }
    }
}

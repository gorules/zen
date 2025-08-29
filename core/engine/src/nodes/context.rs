use crate::nodes::definition::{NodeDataType, TraceDataType};
use crate::nodes::extensions::NodeHandlerExtensions;
use crate::nodes::function::v2::function::Function;
use crate::nodes::result::{NodeResponse, NodeResult};
use crate::NodeError;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use zen_types::variable::Variable;

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
}

impl<NodeData, TraceData> NodeContext<NodeData, TraceData>
where
    NodeData: NodeDataType,
    TraceData: TraceDataType,
{
    pub fn trace<Function>(&self, mutator: Function)
    where
        Function: FnOnce(&mut TraceData),
    {
        if let Some(trace) = &self.trace {
            mutator(&mut *trace.borrow_mut());
        }
    }

    pub fn has_trace(&self) -> bool {
        self.trace.is_some()
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
            node_id: Some(self.id.clone()),
            trace: self.trace.as_ref().map(|v| (*v.borrow()).to_variable()),
            source: error.into(),
        }
    }

    pub fn block_on<Fut>(&self, future: Fut) -> Result<Fut::Output, NodeError>
    where
        Fut: Future,
    {
        let tokio_runtime = self.extensions.tokio_runtime().node_context(self)?;
        Ok(tokio_runtime.block_on(future))
    }

    pub fn try_block_on<Fut, Output>(&self, future: Fut) -> Result<Output, NodeError>
    where
        Fut: Future<Output = Result<Output, NodeError>>,
    {
        self.block_on(future)?
    }

    pub(crate) fn function_runtime(&self) -> Result<&Function, NodeError> {
        self.extensions.function_runtime().node_context(self)
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

pub struct NodeContextBase {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub input: Variable,
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
            node_id: Some(self.id.clone()),
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
            id: value.id.clone(),
            name: value.name.clone(),
            input: value.input.clone(),
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

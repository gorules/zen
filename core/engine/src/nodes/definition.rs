use crate::handler::node::NodeResult;
use crate::nodes::context::NodeContext;
use crate::nodes::function::FunctionNodeTrace;
use crate::nodes::input::InputNodeTrace;
use crate::nodes::output::OutputNodeTrace;
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;
use zen_types::decision::{
    CustomNodeContent, DecisionNodeContent, ExpressionNodeContent, FunctionNodeContent,
    InputNodeContent, OutputNodeContent, TransformAttributes,
};
use zen_types::variable::ToVariable;

pub trait NodeDataType: Clone + Debug + Serialize + for<'de> Deserialize<'de> {}
impl<T> NodeDataType for T where T: Clone + Debug + Serialize + for<'de> Deserialize<'de> {}

pub trait TraceDataType: Clone + Debug + Default + ToVariable {}
impl<T> TraceDataType for T where T: Clone + Debug + Default + ToVariable {}

pub trait NodeHandler {
    type NodeData: NodeDataType;
    type TraceData: TraceDataType;

    fn transform_attributes(
        &self,
        _ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        None
    }

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult;
}

pub struct NodeHandlers {
    pub input: Arc<dyn NodeHandler<NodeData = InputNodeContent, TraceData = InputNodeTrace>>,
    pub output: Arc<dyn NodeHandler<NodeData = OutputNodeContent, TraceData = OutputNodeTrace>>,
    pub function:
        Arc<dyn NodeHandler<NodeData = FunctionNodeContent, TraceData = FunctionNodeTrace>>,
    pub expression:
        Box<dyn NodeHandler<NodeData = ExpressionNodeContent, TraceData = ExpressionNodeContent>>,
}

pub enum NodeHandlerKind {
    Input(),
    Output(Box<dyn NodeHandler<NodeData = OutputNodeContent, TraceData = ()>>),
    Function(Box<dyn NodeHandler<NodeData = FunctionNodeContent, TraceData = ()>>),
    Expression(Box<dyn NodeHandler<NodeData = ExpressionNodeContent, TraceData = ()>>),
    DecisionTable(Box<dyn NodeHandler<NodeData = DecisionNodeContent, TraceData = ()>>),
    Decision(Box<dyn NodeHandler<NodeData = DecisionNodeContent, TraceData = ()>>),
    Custom(Box<dyn NodeHandler<NodeData = CustomNodeContent, TraceData = ()>>),
}

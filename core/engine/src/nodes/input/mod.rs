use crate::handler::node::NodeResult;
use crate::nodes::definition::NodeHandler;
use crate::nodes::NodeContext;
use zen_types::decision::InputNodeContent;
use zen_types::variable::Variable;

pub struct InputNodeHandler;

impl NodeHandler for InputNodeHandler {
    type NodeData = InputNodeContent;
    type TraceData = Variable;

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        ctx.success(ctx.input.clone())
    }
}

pub type InputNodeTrace = Variable;

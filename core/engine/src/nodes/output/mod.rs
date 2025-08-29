use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use crate::nodes::NodeContext;
use zen_types::decision::OutputNodeContent;
use zen_types::variable::Variable;

pub struct OutputNodeHandler;

impl NodeHandler for OutputNodeHandler {
    type NodeData = OutputNodeContent;
    type TraceData = OutputNodeTrace;

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        ctx.success(ctx.input.clone())
    }
}

pub type OutputNodeTrace = Variable;

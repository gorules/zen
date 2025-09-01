use crate::nodes::result::NodeResult;
use crate::nodes::{NodeContext, NodeHandler};
use zen_types::decision::CustomNodeContent;
use zen_types::variable::Variable;

pub use adapter::{
    CustomDecisionNode, CustomNodeAdapter, CustomNodeRequest, DynamicCustomNode, NoopCustomNode,
};

mod adapter;

pub struct CustomNodeHandler;
pub type CustomNodeData = CustomNodeContent;
pub type CustomNodeTrace = Variable;

impl NodeHandler for CustomNodeHandler {
    type NodeData = CustomNodeData;
    type TraceData = CustomNodeTrace;

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let custom_node_request = CustomNodeRequest {
            input: ctx.input.clone(),
            node: CustomDecisionNode {
                id: ctx.id.clone(),
                name: ctx.name.clone(),
                kind: ctx.node.kind.clone(),
                config: ctx.node.config.clone(),
            },
        };

        ctx.block_on(ctx.extensions.custom_node().handle(custom_node_request))?
    }
}

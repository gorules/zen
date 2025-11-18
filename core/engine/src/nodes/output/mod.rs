use crate::decision_graph::cleaner::VariableCleaner;
use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use crate::nodes::NodeContext;
use zen_types::decision::OutputNodeContent;
use zen_types::variable::Variable;

#[derive(Debug, Clone)]
pub struct OutputNodeHandler;

pub type OutputNodeData = OutputNodeContent;
pub type OutputNodeTrace = Variable;

impl NodeHandler for OutputNodeHandler {
    type NodeData = OutputNodeData;
    type TraceData = OutputNodeTrace;

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        if let Some(json_schema) = &ctx.node.schema {
            let cloned_input = ctx.input.deep_clone();
            VariableCleaner::new().clean(&cloned_input);

            let input_json = cloned_input.to_value();
            ctx.validate(json_schema, &input_json)?;
        };

        ctx.success(ctx.input.clone())
    }
}

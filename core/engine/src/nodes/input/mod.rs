pub(crate) mod dates;

use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use crate::nodes::NodeContext;
use zen_types::decision::InputNodeContent;
use zen_types::variable::Variable;

#[derive(Debug, Clone)]
pub struct InputNodeHandler;

pub type InputNodeData = InputNodeContent;
pub type InputNodeTrace = Variable;

impl NodeHandler for InputNodeHandler {
    type NodeData = InputNodeData;
    type TraceData = InputNodeTrace;

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let Some(json_schema) = &ctx.node.schema else {
            let output = dates::DeclaredDates::stringify(&ctx.input);
            return ctx.success(output.unwrap_or_else(|| ctx.input.clone()));
        };
        ctx.validate(json_schema, &ctx.input)?;

        let output = dates::DeclaredDates::convert(&ctx.input, json_schema);
        ctx.success(output.unwrap_or_else(|| ctx.input.clone()))
    }
}

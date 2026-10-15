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
        if let Some(json_schema) = &ctx.node.schema {
            ctx.validate(json_schema, &ctx.input)?;
        }

        // A request typed by an entity is prepared as the entity (its dates
        // dates, as policies read them): given on as it is.
        if ctx.node.target.is_some() {
            return ctx.success(ctx.input.clone());
        }
        let output = dates::DeclaredDates::prepare(&ctx.input, ctx.node.schema.as_deref());
        ctx.success(output.unwrap_or_else(|| ctx.input.clone()))
    }
}

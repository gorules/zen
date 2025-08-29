use std::ops::Deref;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::nodes::definition::NodeHandler;
use crate::nodes::function::v1::runtime::create_runtime;
use crate::nodes::function::v1::script::Script;
use crate::nodes::result::NodeResult;
use crate::nodes::{NodeContext, NodeContextExt};
use serde_json::Value;
use zen_expression::variable::ToVariable;

pub(crate) mod runtime;
mod script;

pub struct FunctionV1NodeHandler;

const MAX_DURATION: Duration = Duration::from_millis(500);

impl NodeHandler for FunctionV1NodeHandler {
    type NodeData = Arc<str>;
    type TraceData = FunctionV1Trace;

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let start = Instant::now();
        let runtime = create_runtime().node_context_message(&ctx, "Failed to create JS Runtime")?;
        let interrupt_handler = Box::new(move || start.elapsed() > MAX_DURATION);

        runtime.set_interrupt_handler(Some(interrupt_handler));

        let mut script = Script::new(runtime.clone());
        let result_response = ctx.block_on(script.call(ctx.node.deref(), &ctx.input))?;

        runtime.set_interrupt_handler(None);

        let response = result_response.node_context(&ctx)?;
        ctx.trace(|t| {
            t.log = response.log;
        });

        ctx.success(response.output)
    }
}

#[derive(Debug, Clone, Default, ToVariable)]
pub struct FunctionV1Trace {
    log: Vec<Value>,
}

use std::ops::Deref;
use std::rc::Rc;
use std::time::Duration;

use crate::handler::node::NodeResult;
use crate::nodes::definition::NodeHandler;
use crate::nodes::function::v2::error::FunctionResult;
use crate::nodes::function::v2::function::{Function, HandlerResponse};
use crate::nodes::function::v2::module::console::Log;
use crate::nodes::function::v2::serde::JsValue;
use crate::nodes::{NodeContext, NodeContextExt};
use ::serde::{Deserialize, Serialize};
use rquickjs::{async_with, CatchResultExt, Object};
use serde_json::json;
use zen_expression::variable::ToVariable;
use zen_types::decision::FunctionContent;

pub(crate) mod error;
pub(crate) mod function;
pub(crate) mod listener;
pub(crate) mod module;
pub(crate) mod serde;

pub struct FunctionHandler {
    function: Rc<Function>,
    trace: bool,
    iteration: u8,
    max_depth: u8,
    max_duration: Duration,
}

pub struct FunctionV2NodeHandler;

impl NodeHandler for FunctionV2NodeHandler {
    type NodeData = FunctionContent;
    type TraceData = FunctionV2Trace;

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let start = std::time::Instant::now();

        // TODO: Smart node omit

        let function = ctx.function_runtime()?;
        let module_name = function.suggest_module_name(ctx.id.deref(), ctx.node.source.deref());

        // TODO: Add duration from configuration
        let max_duration = Duration::from_millis(500);
        let interrupt_handler = Box::new(move || start.elapsed() > max_duration);

        ctx.try_block_on(async {
            function
                .runtime()
                .set_interrupt_handler(Some(interrupt_handler))
                .await;

            self.attach_globals(function).await.node_context(&ctx)?;

            function
                .register_module(&module_name, ctx.node.source.deref())
                .await
                .node_context(&ctx)?;

            let response_result = function
                .call_handler(&module_name, JsValue(ctx.input.clone()))
                .await;

            match response_result {
                Ok(response) => {
                    function.runtime().set_interrupt_handler(None).await;
                    ctx.trace(|t| {
                        t.log = response.logs.clone();
                    });

                    ctx.success(response.data)
                }
                Err(e) => {
                    let log = function.extract_logs().await;
                    ctx.trace(|t| {
                        t.log = log;
                        t.log.push(Log {
                            lines: vec![json!(e.to_string()).to_string()],
                            ms_since_run: start.elapsed().as_millis() as usize,
                        });
                    });

                    ctx.error(e)
                }
            }
        })
    }
}

impl FunctionV2NodeHandler {
    async fn attach_globals(&self, function: &Function) -> FunctionResult {
        async_with!(function.context() => |ctx| {
            let config = Object::new(ctx.clone()).catch(&ctx)?;

            // TODO: Restore configuration
            // config.prop("iteration", self.iteration).catch(&ctx)?;
            // config.prop("maxDepth", self.max_depth).catch(&ctx)?;
            // config.prop("trace", self.trace).catch(&ctx)?;

            ctx.globals().set("config", config).catch(&ctx)?;

            Ok(())
        })
        .await
    }
}

#[derive(Debug, Clone, Default, ToVariable)]
#[serde(rename_all = "camelCase")]
pub struct FunctionV2Trace {
    pub log: Vec<Log>,
}

#[derive(Serialize, Deserialize)]
pub struct FunctionResponse {
    performance: String,
    data: Option<HandlerResponse>,
}

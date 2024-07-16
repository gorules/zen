use std::rc::Rc;
use std::time::Duration;

use ::serde::{Deserialize, Serialize};
use anyhow::anyhow;
use rquickjs::{async_with, CatchResultExt, Object};
use serde_json::json;

use crate::handler::function::error::FunctionResult;
use crate::handler::function::function::{Function, HandlerResponse};
use crate::handler::function::serde::JsValue;
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::{DecisionNodeKind, FunctionNodeContent};

pub(crate) mod error;
pub(crate) mod function;
pub(crate) mod listener;
pub(crate) mod module;
pub(crate) mod serde;

#[derive(Serialize, Deserialize)]
pub struct FunctionResponse {
    performance: String,
    data: Option<HandlerResponse>,
}

pub struct FunctionHandler {
    function: Rc<Function>,
    trace: bool,
    iteration: u8,
    max_depth: u8,
}

static MAX_DURATION: Duration = Duration::from_millis(5_000);

impl FunctionHandler {
    pub fn new(function: Rc<Function>, trace: bool, iteration: u8, max_depth: u8) -> Self {
        Self {
            function,
            trace,
            iteration,
            max_depth,
        }
    }

    pub async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::FunctionNode { content } => match content {
                FunctionNodeContent::Version2(content) => Ok(content),
                _ => Err(anyhow!("Unexpected node type")),
            },
            _ => Err(anyhow!("Unexpected node type")),
        }?;
        let start = std::time::Instant::now();

        let module_name = self
            .function
            .suggest_module_name(request.node.id.as_str(), request.node.name.as_str());
        let interrupt_handler = Box::new(move || start.elapsed() > MAX_DURATION);
        self.function
            .runtime()
            .set_interrupt_handler(Some(interrupt_handler))
            .await;

        self.attach_globals()
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        self.function
            .register_module(&module_name, content.source.as_str())
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        let response = self
            .function
            .call_handler(&module_name, JsValue(request.input.clone()))
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        self.function.runtime().set_interrupt_handler(None).await;

        Ok(NodeResponse {
            output: response.data,
            trace_data: self.trace.then(|| json!({ "log": response.logs })),
        })
    }

    async fn attach_globals(&self) -> FunctionResult {
        async_with!(self.function.context() => |ctx| {
            let config = Object::new(ctx.clone()).catch(&ctx)?;

            config.prop("iteration", self.iteration).catch(&ctx)?;
            config.prop("maxDepth", self.max_depth).catch(&ctx)?;
            config.prop("trace", self.trace).catch(&ctx)?;

            ctx.globals().set("config", config).catch(&ctx)?;

            Ok(())
        })
        .await
    }
}

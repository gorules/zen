use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use rquickjs::Runtime;
use serde_json::json;

use crate::handler::function::script::Script;
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::DecisionNodeKind;
use crate::ZEN_CONFIG;

mod js_value;
pub(crate) mod runtime;
mod script;

pub struct FunctionHandler {
    trace: bool,
    runtime: Runtime,
}

impl FunctionHandler {
    pub fn new(trace: bool, runtime: Runtime) -> Self {
        Self { trace, runtime }
    }

    pub async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let max_duration_millis = ZEN_CONFIG.function_timeout.load(Ordering::Relaxed);
        let max_duration = Duration::from_millis(max_duration_millis);

        let content = match &request.node.kind {
            DecisionNodeKind::FunctionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let start = Instant::now();
        let interrupt_handler = Box::new(move || start.elapsed() > max_duration);
        self.runtime.set_interrupt_handler(Some(interrupt_handler));

        let mut script = Script::new(self.runtime.clone());
        let result_response = script.call(content, &request.input).await;

        self.runtime.set_interrupt_handler(None);

        let response = result_response?;
        Ok(NodeResponse {
            output: response.output,
            trace_data: self.trace.then(|| json!({ "log": response.log })),
        })
    }
}

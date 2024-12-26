use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::handler::function_v1::script::Script;
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::{DecisionNodeKind, FunctionNodeContent};
use crate::ZEN_CONFIG;
use anyhow::anyhow;
use rquickjs::Runtime;
use serde_json::json;

pub(crate) mod runtime;
mod script;

pub struct FunctionHandler {
    trace: bool,
    runtime: Runtime,
    max_duration: Duration,
}

impl FunctionHandler {
    pub fn new(trace: bool, runtime: Runtime) -> Self {
        let max_duration_millis = ZEN_CONFIG.function_timeout.load(Ordering::Relaxed);

        Self {
            trace,
            runtime,
            max_duration: Duration::from_millis(max_duration_millis),
        }
    }

    pub async fn handle(&self, request: NodeRequest) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::FunctionNode { content } => match content {
                FunctionNodeContent::Version1(content) => Ok(content),
                _ => Err(anyhow!("Unexpected node type")),
            },
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let start = Instant::now();
        let max_duration = self.max_duration.clone();
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

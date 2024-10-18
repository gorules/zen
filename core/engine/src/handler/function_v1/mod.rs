use std::time::{Duration, Instant};

use crate::handler::function_v1::script::Script;
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::{DecisionNodeKind, FunctionNodeContent};
use anyhow::anyhow;
use rquickjs::Runtime;
use serde_json::json;

pub(crate) mod runtime;
mod script;

pub struct FunctionHandler {
    trace: bool,
    runtime: Runtime,
}

static MAX_DURATION: Duration = Duration::from_millis(500);

impl FunctionHandler {
    pub fn new(trace: bool, runtime: Runtime) -> Self {
        Self { trace, runtime }
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
        let interrupt_handler = Box::new(move || start.elapsed() > MAX_DURATION);
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

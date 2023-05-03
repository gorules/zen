use anyhow::anyhow;
use std::time::Duration;

use serde_json::{json, Value};

use crate::handler::function::script::{EvaluateResponse, Script};
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::DecisionNodeKind;

mod script;
mod vm;

async fn evaluate(source: &str, args: &Value) -> anyhow::Result<EvaluateResponse> {
    let mut script = Script::new().with_timeout(Duration::from_millis(50));
    script.call(source, args).await
}

pub struct FunctionHandler {
    trace: bool,
}

impl FunctionHandler {
    pub fn new(trace: bool) -> Self {
        Self { trace }
    }

    pub async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::FunctionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let result = evaluate(content.as_str(), &request.input).await?;

        Ok(NodeResponse {
            output: result.output,
            trace_data: self.trace.then(|| json!({ "log": result.log })),
        })
    }
}

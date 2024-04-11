use napi::anyhow::anyhow;
use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction};

use zen_engine::handler::custom_node_adapter::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::handler::node::{NodeResponse, NodeResult};

use crate::types::{ZenEngineHandlerRequest, ZenEngineHandlerResponse};

#[derive(Default)]
pub(crate) struct CustomNode {
    function: Option<ThreadsafeFunction<ZenEngineHandlerRequest, ErrorStrategy::Fatal>>,
}

impl CustomNode {
    pub fn new(tsf: ThreadsafeFunction<ZenEngineHandlerRequest, ErrorStrategy::Fatal>) -> Self {
        Self {
            function: Some(tsf),
        }
    }
}

impl CustomNodeAdapter for CustomNode {
    async fn handle(&self, request: CustomNodeRequest<'_>) -> NodeResult {
        let Some(function) = &self.function else {
            return Err(anyhow!("Custom function is undefined"));
        };

        let node_data = crate::types::DecisionNode::from(request.node);

        let promise: Promise<ZenEngineHandlerResponse> = function
            .clone()
            .call_async(ZenEngineHandlerRequest {
                input: request.input.clone(),
                node: node_data,
            })
            .await
            .map_err(|err| anyhow!(err.reason))?;

        let result = promise.await.map_err(|err| anyhow!(err.reason))?;

        Ok(NodeResponse {
            output: result.output,
            trace_data: result.trace_data,
        })
    }
}

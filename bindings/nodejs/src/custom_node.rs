use napi::anyhow::anyhow;
use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction};
use napi::{Env, JsFunction};

use zen_engine::handler::custom_node_adapter::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::handler::node::{NodeResponse, NodeResult};

use crate::types::{ZenEngineHandlerRequest, ZenEngineHandlerResponse};

pub(crate) struct CustomNode {
    function: Option<ThreadsafeFunction<ZenEngineHandlerRequest, ErrorStrategy::Fatal>>,
}

impl Default for CustomNode {
    fn default() -> Self {
        Self { function: None }
    }
}

impl CustomNode {
    pub fn try_new(env: &mut Env, function: JsFunction) -> napi::Result<Self> {
        let mut tsf = function.create_threadsafe_function(
            0,
            |cx: ThreadSafeCallContext<ZenEngineHandlerRequest>| Ok(vec![cx.value]),
        )?;

        tsf.unref(env)?;

        Ok(Self {
            function: Some(tsf),
        })
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

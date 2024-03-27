use napi::anyhow::{anyhow, Context};
use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction};
use napi::{Env, JsFunction};
use serde::Serialize;
use serde_json::Value;
use zen_engine::handler::custom_node_adapter::CustomNodeAdapter;

use crate::types::ZenEngineHandlerRequest;
use zen_engine::handler::node::{NodeRequest, NodeResult};
use zen_engine::model::DecisionNode;

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

#[derive(Serialize)]
struct FunctionRequestData<'a, 'b> {
    input: &'a Value,
    node: &'b DecisionNode,
    iteration: u8,
}

impl CustomNodeAdapter for CustomNode {
    async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let Some(function) = &self.function else {
            return Err(anyhow!("Custom function is undefined"));
        };

        let node_data = crate::types::DecisionNode::try_from(request.node.clone()).unwrap();

        let promise: Promise<Option<Value>> = function
            .clone()
            .call_async(ZenEngineHandlerRequest {
                input: request.input.clone(),
                node: node_data,
                iteration: request.iteration,
            })
            .await
            .map_err(|err| anyhow!(err.reason))?;

        let result = promise
            .await
            .map_err(|err| anyhow!(err.reason))?
            .unwrap_or(Value::Null);

        serde_json::from_value(result).context("Failed to deserialize return data")
    }
}

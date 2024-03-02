use napi::anyhow::anyhow;
use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction};
use napi::{Env, JsFunction};
use serde_json::{json, Value};

use zen_engine::handler::node::{NodeRequest, NodeResponse, NodeResult};
use zen_engine::model::custom_node_adapter::CustomNodeAdapter;

pub(crate) struct CustomNode {
    function: Option<ThreadsafeFunction<Value, ErrorStrategy::Fatal>>,
}

impl Default for CustomNode {
    fn default() -> Self {
        Self { function: None }
    }
}

impl CustomNode {
    pub fn try_new(env: &mut Env, function: JsFunction) -> napi::Result<Self> {
        let mut tsf = function
            .create_threadsafe_function(0, |cx: ThreadSafeCallContext<Value>| Ok(vec![cx.value]))?;

        tsf.unref(env)?;

        Ok(Self {
            function: Some(tsf),
        })
    }
}

impl CustomNodeAdapter for CustomNode {
    async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let Some(function) = &self.function else {
            return Err(anyhow!("Custom function is undefined"));
        };

        let decision_content = serde_json::to_value(request.node).unwrap();
        let input = request.input.clone();

        let promise: Promise<Option<Value>> = function
            .clone()
            .call_async(json!({"input": input, "node": decision_content}))
            .await
            .map_err(|err| anyhow!(err.reason))?;

        let result = promise
            .await
            .map_err(|err| anyhow!(err.reason))?
            .unwrap_or(Value::Null);

        Ok(NodeResponse {
            output: result,
            trace_data: None,
        })
    }
}

use crate::engine::JsZenEvaluateOptions;
use crate::loader::JsDecisionLoader;
use napi::anyhow::anyhow;
use napi::tokio;
use napi_derive::napi;
use serde_json::Value;
use std::sync::Arc;
use zen_engine::{Decision, EvaluationOptions};

#[napi(js_name = "ZenDecision")]
pub struct JsZenDecision(pub(crate) Arc<Decision<JsDecisionLoader>>);

impl From<Decision<JsDecisionLoader>> for JsZenDecision {
    fn from(value: Decision<JsDecisionLoader>) -> Self {
        Self(value.into())
    }
}

#[napi]
impl JsZenDecision {
    #[napi(constructor)]
    pub fn new() -> napi::Result<Self> {
        Err(anyhow!("Private constructor").into())
    }

    #[napi]
    pub async fn evaluate(
        &self,
        context: Value,
        opts: Option<JsZenEvaluateOptions>,
    ) -> napi::Result<Value> {
        let decision = self.0.clone();
        let result = tokio::spawn(async move {
            let options = opts.unwrap_or_default();
            futures::executor::block_on(decision.evaluate_with_opts(
                &context,
                EvaluationOptions {
                    max_depth: options.max_depth,
                    trace: options.trace,
                },
            ))
        })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
        .map_err(|e| anyhow!(e))?;

        Ok(serde_json::to_value(&result)?)
    }
}

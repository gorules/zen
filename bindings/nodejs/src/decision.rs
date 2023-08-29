use crate::engine::ZenEvaluateOptions;
use crate::loader::DecisionLoader;
use napi::anyhow::anyhow;
use napi::tokio;
use napi_derive::napi;
use serde_json::Value;
use std::sync::Arc;
use zen_engine::{Decision, EvaluationOptions};

#[napi]
pub struct ZenDecision(pub(crate) Arc<Decision<DecisionLoader>>);

impl From<Decision<DecisionLoader>> for ZenDecision {
    fn from(value: Decision<DecisionLoader>) -> Self {
        Self(value.into())
    }
}

#[napi]
impl ZenDecision {
    #[napi(constructor)]
    pub fn new() -> napi::Result<Self> {
        Err(anyhow!("Private constructor").into())
    }

    #[napi]
    pub async fn evaluate(
        &self,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
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
        .map_err(|e| {
            anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
        })?;

        Ok(serde_json::to_value(&result)?)
    }

    #[napi]
    pub fn validate(&self) -> napi::Result<()> {
        let decision = self.0.clone();
        let result = decision
            .validate()
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

        Ok(result)
    }
}

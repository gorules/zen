use crate::custom_node::CustomNode;
use crate::engine::ZenEvaluateOptions;
use crate::loader::DecisionLoader;
use crate::mt::spawn_worker;
use crate::safe_result::SafeResult;
use crate::types::ZenEngineResponse;
use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::Value;
use std::sync::Arc;
use zen_engine::{Decision, EvaluationOptions};

#[napi]
pub struct ZenDecision(pub(crate) Arc<Decision<DecisionLoader, CustomNode>>);

impl From<Decision<DecisionLoader, CustomNode>> for ZenDecision {
    fn from(value: Decision<DecisionLoader, CustomNode>) -> Self {
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
    ) -> napi::Result<ZenEngineResponse> {
        let decision = self.0.clone();
        let result = spawn_worker(move || {
            let options = opts.unwrap_or_default();

            async move {
                decision
                    .evaluate_with_opts(
                        &context,
                        EvaluationOptions {
                            max_depth: options.max_depth,
                            trace: options.trace,
                        },
                    )
                    .await
            }
        })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
        .map_err(|e| {
            anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
        })?;

        Ok(ZenEngineResponse::from(result))
    }

    #[napi(ts_return_type = "Promise<SafeResult<ZenEngineResponse>>")]
    pub async fn safe_evaluate(
        &self,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
    ) -> SafeResult<ZenEngineResponse> {
        self.evaluate(context, opts).await.into()
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

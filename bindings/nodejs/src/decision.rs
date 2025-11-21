use crate::engine::ZenEvaluateOptions;
use crate::mt::spawn_worker;
use crate::safe_result::SafeResult;
use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::Value;
use std::sync::Arc;
use zen_engine::Decision;

#[napi]
pub struct ZenDecision(pub(crate) Arc<Decision>);

impl From<Decision> for ZenDecision {
    fn from(value: Decision) -> Self {
        Self(value.into())
    }
}

#[napi]
impl ZenDecision {
    #[napi(constructor)]
    pub fn new() -> napi::Result<Self> {
        Err(anyhow!("Private constructor").into())
    }

    #[napi(ts_return_type = "Promise<ZenEngineResponse>")]
    pub async fn evaluate(
        &self,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
    ) -> napi::Result<Value> {
        let decision = self.0.clone();
        let result = spawn_worker(move || {
            let options = opts.unwrap_or_default();

            async move {
                decision
                    .evaluate_serialized(context.into(), options.into())
                    .await
            }
        })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
        .map_err(|e| anyhow!(e))?;

        Ok(result)
    }

    #[napi(
        ts_return_type = "{ success: true, data: ZenEngineResponse } | { success: false; error: any; }"
    )]
    pub async fn safe_evaluate(
        &self,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
    ) -> SafeResult<Value> {
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

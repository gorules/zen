use crate::engine::ZenEvaluateOptions;
use crate::error::ZenError;
use crate::types::{JsonBuffer, ZenEngineResponse};
use serde_json::Value;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::task;
use zen_engine::Decision;

#[derive(uniffi::Object)]
pub struct ZenDecision {
    decision: Arc<Decision>,
}

impl From<Decision> for ZenDecision {
    fn from(value: Decision) -> Self {
        Self {
            decision: Arc::new(value),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl ZenDecision {
    pub async fn evaluate(
        &self,
        context: JsonBuffer,
        options: Option<ZenEvaluateOptions>,
    ) -> Result<ZenEngineResponse, ZenError> {
        let options = options.unwrap_or_default();
        let context: Value = context.try_into()?;

        let decision = self.decision.clone();

        // Use spawn_blocking to run the non-Send code synchronously
        let response = task::spawn_blocking(move || {
            // The blocking code that uses non-Send types
            Handle::current().block_on(async move {
                decision
                    .evaluate_with_opts(context.into(), options.into())
                    .await
                    .map(|response| ZenEngineResponse::try_from(response))
                    .map_err(|err| {
                        ZenError::EvaluationError(
                            serde_json::to_string(&err.as_ref())
                                .unwrap_or_else(|_| err.to_string()),
                        )
                    })
            })
        })
        .await
        .map_err(|e| ZenError::EvaluationError(format!("Task failed: {:?}", e)))???;

        Ok(response)
    }

    pub fn validate(&self) -> Result<(), ZenError> {
        self.decision.validate().map_err(|e| {
            ZenError::ValidationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        })
    }
}

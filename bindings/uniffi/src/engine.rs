use crate::custom_node::{
    NoopCustomNodeCallback, ZenCustomNodeCallback, ZenCustomNodeCallbackWrapper,
};
use crate::decision::ZenDecision;
use crate::error::ZenError;
use crate::loader::{
    NoopDecisionLoader, ZenDecisionLoaderCallback, ZenDecisionLoaderCallbackWrapper,
};
use crate::types::{JsonBuffer, ZenEngineResponse};
use serde_json::Value;
use std::sync::Arc;
use tokio::runtime::Handle;
use tokio::task;
use zen_engine::{DecisionEngine, EvaluationOptions};

#[derive(uniffi::Object)]
pub(crate) struct ZenEngine {
    engine: Arc<DecisionEngine<ZenDecisionLoaderCallbackWrapper, ZenCustomNodeCallbackWrapper>>,
}

#[derive(uniffi::Record)]
pub struct ZenEvaluateOptions {
    pub max_depth: Option<u8>,
    pub trace: Option<bool>,
}

impl Default for ZenEvaluateOptions {
    fn default() -> Self {
        Self {
            max_depth: Some(5),
            trace: Some(false),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl ZenEngine {
    #[uniffi::constructor]
    pub fn new(
        loader: Option<Box<dyn ZenDecisionLoaderCallback>>,
        custom_node: Option<Box<dyn ZenCustomNodeCallback>>,
    ) -> Self {
        Self {
            engine: Arc::new(DecisionEngine::new(
                Arc::new(ZenDecisionLoaderCallbackWrapper(
                    loader.unwrap_or_else(|| Box::new(NoopDecisionLoader)),
                )),
                Arc::new(ZenCustomNodeCallbackWrapper(
                    custom_node.unwrap_or_else(|| Box::new(NoopCustomNodeCallback)),
                )),
            )),
        }
    }

    pub async fn evaluate(
        &self,
        key: String,
        context: Arc<JsonBuffer>,
        options: Option<ZenEvaluateOptions>,
    ) -> Result<ZenEngineResponse, ZenError> {
        let options = options.unwrap_or_default();
        let context: Value = context.to_value();

        let engine = self.engine.clone();
        let evaluation_options = EvaluationOptions {
            max_depth: options.max_depth,
            trace: options.trace,
        };

        // Use spawn_blocking to run the non-Send code synchronously
        let response = task::spawn_blocking(move || {
            // The blocking code that uses non-Send types
            Handle::current().block_on(async move {
                engine
                    .evaluate_with_opts(key, context.into(), evaluation_options)
                    .await
                    .map(|response| ZenEngineResponse::from(response))
                    .map_err(|err| {
                        ZenError::EvaluationError(
                            serde_json::to_string(&err.as_ref())
                                .unwrap_or_else(|_| err.to_string()),
                        )
                    })
            })
        })
        .await
        .map_err(|e| ZenError::EvaluationError(format!("Task failed: {:?}", e)))??;

        Ok(response)
    }

    pub fn create_decision(&self, content: Arc<JsonBuffer>) -> Result<ZenDecision, ZenError> {
        let decision = self.engine.create_decision(Arc::new(
            serde_json::from_value(content.to_value())
                .map_err(|_| ZenError::JsonDeserializationFailed)?,
        ));

        Ok(ZenDecision::from(decision))
    }

    pub async fn get_decision(&self, key: String) -> Result<ZenDecision, ZenError> {
        let engine = self.engine.clone();

        // Use spawn_blocking to run the non-Send code synchronously
        let decision = task::spawn_blocking(move || {
            // The blocking code that uses non-Send types
            Handle::current().block_on(async move {
                engine
                    .get_decision(&key)
                    .await
                    .map_err(|e| ZenError::LoaderInternalError {
                        key,
                        details: e.to_string(),
                    })
                    .map(ZenDecision::from)
            })
        })
        .await
        .map_err(|e| ZenError::EvaluationError(format!("Task failed: {:?}", e)))??;

        Ok(decision)
    }
}

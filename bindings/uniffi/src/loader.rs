use crate::error::ZenError;
use crate::types::JsonBuffer;
use std::future::Future;
use std::sync::Arc;
use uniffi::deps::anyhow::anyhow;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;

#[uniffi::export(callback_interface)]
#[async_trait::async_trait]
pub trait ZenDecisionLoaderCallback: Send + Sync {
    async fn load(&self, key: String) -> Result<Option<JsonBuffer>, ZenError>;
}

pub struct NoopDecisionLoader;

#[async_trait::async_trait]
impl ZenDecisionLoaderCallback for NoopDecisionLoader {
    async fn load(&self, _: String) -> Result<Option<JsonBuffer>, ZenError> {
        Err(ZenError::Zero)
    }
}

pub struct ZenDecisionLoaderCallbackWrapper(pub Box<dyn ZenDecisionLoaderCallback>);

impl DecisionLoader for ZenDecisionLoaderCallbackWrapper {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            let maybe_json_buffer = match self.0.load(key.into()).await {
                Ok(r) => r,
                Err(error) => {
                    return Err(Box::new(LoaderError::Internal {
                        key: key.to_string(),
                        source: anyhow!(error),
                    }));
                }
            };

            let Some(json_buffer) = maybe_json_buffer else {
                return Err(Box::new(LoaderError::NotFound(key.to_string())));
            };

            let decision_content: DecisionContent =
                serde_json::from_slice(json_buffer.0.as_slice()).map_err(|e| {
                    LoaderError::Internal {
                        key: key.to_string(),
                        source: anyhow!(e),
                    }
                })?;

            Ok(Arc::new(decision_content))
        }
    }
}

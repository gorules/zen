use crate::error::ZenError;
use crate::types::JsonBuffer;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use uniffi::deps::anyhow::anyhow;
use zen_engine::loader::{
    DecisionLoader, DynamicLoader, LoaderConfig, LoaderError, LoaderResponse,
};
use zen_engine::model::DecisionContent;

#[uniffi::export(with_foreign)]
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

#[derive(uniffi::Enum)]
pub enum ZenLoader {
    Callback {
        callback: Arc<dyn ZenDecisionLoaderCallback>,
    },
    Static {
        content: HashMap<String, JsonBuffer>,
    },
    Filesystem {
        path: String,
    },
    Zip {
        bytes: Vec<u8>,
    },
}

impl ZenLoader {
    pub fn into_dynamic_loader(self) -> Result<DynamicLoader, ZenError> {
        let config = match self {
            ZenLoader::Callback { callback } => {
                return Ok(Arc::new(ZenDecisionLoaderCallbackWrapper(callback)));
            }
            ZenLoader::Static { content } => {
                let content = content
                    .into_iter()
                    .map(|(key, buffer)| {
                        let decision_content: DecisionContent =
                            serde_json::from_slice(buffer.0.as_slice())
                                .map_err(|_| ZenError::JsonDeserializationFailed)?;
                        Ok((key, decision_content))
                    })
                    .collect::<Result<HashMap<_, _>, ZenError>>()?;

                LoaderConfig::Static { content }
            }
            ZenLoader::Filesystem { path } => LoaderConfig::Filesystem { path },
            ZenLoader::Zip { bytes } => LoaderConfig::Zip { bytes },
        };

        config
            .into_loader()
            .map_err(|e| ZenError::ValidationError(e.to_string()))
    }
}

pub struct ZenDecisionLoaderCallbackWrapper(pub Arc<dyn ZenDecisionLoaderCallback>);

impl Debug for ZenDecisionLoaderCallbackWrapper {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ZenDecisionLoaderCallbackWrapper")
    }
}

impl DecisionLoader for ZenDecisionLoaderCallbackWrapper {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move {
            let maybe_json_buffer = match self.0.load(key.into()).await {
                Ok(r) => r,
                Err(error) => {
                    return Err(LoaderError::Internal {
                        key: key.to_string(),
                        source: anyhow!(error),
                    });
                }
            };

            let Some(json_buffer) = maybe_json_buffer else {
                return Err(LoaderError::NotFound(key.to_string()));
            };

            let decision_content: DecisionContent =
                serde_json::from_slice(json_buffer.0.as_slice()).map_err(|e| {
                    LoaderError::Internal {
                        key: key.to_string(),
                        source: anyhow!(e),
                    }
                })?;

            Ok(Arc::new(decision_content))
        })
    }
}

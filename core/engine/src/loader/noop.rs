use crate::loader::{DecisionLoader, LoaderError, LoaderResponse};
use anyhow::anyhow;
use async_trait::async_trait;

/// Default loader which always fails
#[derive(Default, Debug)]
pub struct NoopLoader;

#[async_trait]
impl DecisionLoader for NoopLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        Err(LoaderError::Internal {
            key: key.to_string(),
            source: anyhow!("Loader is no-op"),
        }
        .into())
    }
}

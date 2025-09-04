use crate::loader::{DecisionLoader, LoaderError, LoaderResponse};
use anyhow::anyhow;
use std::future::Future;
use std::pin::Pin;

/// Default loader which always fails
#[derive(Default, Debug)]
pub struct NoopLoader;

impl DecisionLoader for NoopLoader {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move {
            Err(LoaderError::Internal {
                key: key.to_string(),
                source: anyhow!("Loader is no-op"),
            }
            .into())
        })
    }
}

use std::future::Future;

use anyhow::anyhow;

use crate::loader::{DecisionLoader, LoaderError, LoaderResponse};

/// Default loader which always fails
#[derive(Default, Debug)]
pub struct NoopLoader;

impl DecisionLoader for NoopLoader {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            Err(LoaderError::Internal {
                key: key.to_string(),
                source: anyhow!("Loader is no-op"),
            }
            .into())
        }
    }
}

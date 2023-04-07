mod closure;
mod filesystem;
mod memory;
mod noop;

pub use closure::ClosureLoader;
pub use filesystem::{FilesystemLoader, FilesystemLoaderOptions};
pub use memory::MemoryLoader;
pub use noop::NoopLoader;

use async_trait::async_trait;

use crate::model::DecisionContent;
use std::fmt::Debug;
use std::sync::Arc;
use thiserror::Error;

pub type LoaderResult<T> = Result<T, LoaderError>;
pub type LoaderResponse = LoaderResult<Arc<DecisionContent>>;

#[async_trait]
pub trait DecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse;
}

#[derive(Error, Debug)]
pub enum LoaderError {
    #[error("Loader did not find item with key {0}")]
    NotFound(String),
    #[error("Loader failed internally on key {key}: {source}.")]
    Internal {
        key: String,
        #[source]
        source: anyhow::Error,
    },
}

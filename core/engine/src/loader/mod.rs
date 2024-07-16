use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;

use thiserror::Error;

pub use cached::CachedLoader;
pub use closure::ClosureLoader;
pub use filesystem::{FilesystemLoader, FilesystemLoaderOptions};
pub use memory::MemoryLoader;
pub use noop::NoopLoader;

use crate::model::DecisionContent;

mod cached;
mod closure;
mod filesystem;
mod memory;
mod noop;

pub type LoaderResult<T> = Result<T, Box<LoaderError>>;
pub type LoaderResponse = LoaderResult<Arc<DecisionContent>>;

/// Trait used for implementing a loader for decisions
pub trait DecisionLoader {
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a;
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

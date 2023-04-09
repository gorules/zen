use crate::loader::{DecisionLoader, LoaderResponse};
use async_trait::async_trait;
use std::future::Future;

/// Loads decisions using an async closure
#[derive(Debug)]
pub struct ClosureLoader<F>
where
    F: Sync + Send,
{
    closure: F,
}

impl<F, O> ClosureLoader<F>
where
    F: Fn(&str) -> O + Sync + Send,
    O: Future<Output = LoaderResponse> + Send,
{
    pub fn new(closure: F) -> Self {
        Self { closure }
    }
}

#[async_trait]
impl<F, O> DecisionLoader for ClosureLoader<F>
where
    F: Fn(&str) -> O + Sync + Send,
    O: Future<Output = LoaderResponse> + Send,
{
    async fn load(&self, key: &str) -> LoaderResponse {
        let closure = &self.closure;
        closure(key).await
    }
}

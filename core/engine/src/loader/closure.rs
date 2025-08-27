use crate::loader::{DecisionLoader, LoaderResponse};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;

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
    F: Fn(String) -> O + Sync + Send,
    O: Future<Output = LoaderResponse> + Send,
{
    pub fn new(closure: F) -> Self {
        Self { closure }
    }
}

impl<F, O> DecisionLoader for ClosureLoader<F>
where
    F: Fn(String) -> O + Sync + Send + Debug,
    O: Future<Output = LoaderResponse> + Send,
{
    fn load<'a>(&'a self, key: &'a str) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a>> {
        Box::pin(async move {
            let closure = &self.closure;
            closure(key.to_string()).await
        })
    }
}

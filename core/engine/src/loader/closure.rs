use std::future::Future;

use crate::loader::{DecisionLoader, LoaderResponse};

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
    F: Fn(String) -> O + Sync + Send,
    O: Future<Output = LoaderResponse> + Send,
{
    fn load<'a>(&'a self, key: &'a str) -> impl Future<Output = LoaderResponse> + 'a {
        async move {
            let closure = &self.closure;
            closure(key.to_string()).await
        }
    }
}

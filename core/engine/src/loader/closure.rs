use crate::loader::{DecisionLoader, LoaderResponse};
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::pin::Pin;

/// Loads decisions using an async closure
pub struct ClosureLoader<F>
where
    F: Sync + Send,
{
    closure: F,
}

impl<T> Debug for ClosureLoader<T>
where
    T: Sync + Send,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ClosureLoader")
    }
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
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move {
            let closure = &self.closure;
            closure(key.to_string()).await
        })
    }
}

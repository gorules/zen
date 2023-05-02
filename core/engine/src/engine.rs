use crate::decision::Decision;
use crate::loader::{ClosureLoader, DecisionLoader, LoaderResponse, LoaderResult, NoopLoader};
use crate::model::DecisionContent;

use serde_json::Value;
use std::future::Future;

use crate::handler::graph::DecisionGraphResponse;
use crate::EvaluationError;
use std::sync::Arc;

/// Structure used for generating and evaluating JDM decisions
#[derive(Debug, Clone)]
pub struct DecisionEngine<L>
where
    L: DecisionLoader,
{
    loader: Arc<L>,
}

#[derive(Debug, Default)]
pub struct EvaluationOptions {
    pub trace: Option<bool>,
    pub max_depth: Option<u8>,
}

impl Default for DecisionEngine<NoopLoader> {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader::default()),
        }
    }
}

impl<F, O> DecisionEngine<ClosureLoader<F>>
where
    F: Fn(&str) -> O + Sync + Send,
    O: Future<Output = LoaderResponse> + Send,
{
    pub fn async_loader(loader: F) -> Self {
        Self {
            loader: Arc::new(ClosureLoader::new(loader)),
        }
    }
}

impl<L: DecisionLoader> DecisionEngine<L> {
    pub fn new<Loader>(loader: Loader) -> Self
    where
        Loader: Into<Arc<L>>,
    {
        Self {
            loader: loader.into(),
        }
    }

    pub fn new_arc(loader: Arc<L>) -> Self {
        Self { loader }
    }

    /// Evaluates a decision through loader using a key
    pub async fn evaluate<K>(
        &self,
        key: K,
        context: &Value,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        self.evaluate_with_opts(key, context, Default::default())
            .await
    }

    /// Evaluates a decision through loader using a key with advanced options
    pub async fn evaluate_with_opts<K>(
        &self,
        key: K,
        context: &Value,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        let content = self.loader.load(key.as_ref()).await?;
        let decision = self.create_decision(content);
        decision.evaluate_with_opts(context, options).await
    }

    /// Creates a decision from DecisionContent, exists for easier binding creation
    pub fn create_decision(&self, content: Arc<DecisionContent>) -> Decision<L> {
        Decision::from(content).with_loader(self.loader.clone())
    }

    /// Retrieves a decision based on the loader
    pub async fn get_decision(&self, key: &str) -> LoaderResult<Decision<L>> {
        let content = self.loader.load(key).await?;
        Ok(self.create_decision(content))
    }
}

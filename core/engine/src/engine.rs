use std::future::Future;
use std::sync::Arc;

use serde_json::Value;

use crate::decision::Decision;
use crate::handler::graph::DecisionGraphResponse;
use crate::loader::{ClosureLoader, DecisionLoader, LoaderResponse, LoaderResult, NoopLoader};
use crate::model::custom_node_adapter::{CustomNodeAdapter, NoopCustomNode};
use crate::model::DecisionContent;
use crate::EvaluationError;

/// Structure used for generating and evaluating JDM decisions
#[derive(Debug, Clone)]
pub struct DecisionEngine<Loader, Adapter>
where
    Loader: DecisionLoader,
    Adapter: CustomNodeAdapter,
{
    loader: Arc<Loader>,
    adapter: Arc<Adapter>,
}

#[derive(Debug, Default)]
pub struct EvaluationOptions {
    pub trace: Option<bool>,
    pub max_depth: Option<u8>,
}

impl Default for DecisionEngine<NoopLoader, NoopCustomNode> {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
        }
    }
}

impl<L: DecisionLoader, A: CustomNodeAdapter> DecisionEngine<L, A> {
    pub fn new(loader: Arc<L>, adapter: Arc<A>) -> Self {
        Self { loader, adapter }
    }

    pub fn with_adapter<Adapter>(self, adapter: Arc<Adapter>) -> DecisionEngine<L, Adapter>
    where
        Adapter: CustomNodeAdapter,
    {
        DecisionEngine {
            loader: self.loader,
            adapter,
        }
    }

    pub fn with_loader<Loader>(self, loader: Arc<Loader>) -> DecisionEngine<Loader, A>
    where
        Loader: DecisionLoader,
    {
        DecisionEngine {
            loader,
            adapter: self.adapter,
        }
    }

    pub fn with_closure_loader<F, O>(self, loader: F) -> DecisionEngine<ClosureLoader<F>, A>
    where
        F: Fn(String) -> O + Sync + Send,
        O: Future<Output = LoaderResponse> + Send,
    {
        DecisionEngine {
            loader: Arc::new(ClosureLoader::new(loader)),
            adapter: self.adapter,
        }
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
    pub fn create_decision(&self, content: Arc<DecisionContent>) -> Decision<L, A> {
        Decision::from(content)
            .with_loader(self.loader.clone())
            .with_adapter(self.adapter.clone())
    }

    /// Retrieves a decision based on the loader
    pub async fn get_decision(&self, key: &str) -> LoaderResult<Decision<L, A>> {
        let content = self.loader.load(key).await?;
        Ok(self.create_decision(content))
    }
}

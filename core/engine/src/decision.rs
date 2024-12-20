use crate::engine::EvaluationOptions;
use crate::handler::custom_node_adapter::{CustomNodeAdapter, NoopCustomNode};
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::loader::{CachedLoader, DecisionLoader, NoopLoader};
use crate::model::DecisionContent;
use crate::util::validator_cache::ValidatorCache;
use crate::{DecisionGraphValidationError, EvaluationError};
use std::sync::Arc;
use zen_expression::variable::Variable;

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision<Loader, CustomNode>
where
    Loader: DecisionLoader + 'static,
    CustomNode: CustomNodeAdapter + 'static,
{
    content: Arc<DecisionContent>,
    loader: Arc<Loader>,
    adapter: Arc<CustomNode>,

    validator_cache: ValidatorCache,
}

impl From<DecisionContent> for Decision<NoopLoader, NoopCustomNode> {
    fn from(value: DecisionContent) -> Self {
        Self {
            content: value.into(),
            loader: NoopLoader::default().into(),
            adapter: NoopCustomNode::default().into(),

            validator_cache: Default::default(),
        }
    }
}

impl From<Arc<DecisionContent>> for Decision<NoopLoader, NoopCustomNode> {
    fn from(value: Arc<DecisionContent>) -> Self {
        Self {
            content: value,
            loader: NoopLoader::default().into(),
            adapter: NoopCustomNode::default().into(),

            validator_cache: Default::default(),
        }
    }
}

impl<L, A> Decision<L, A>
where
    L: DecisionLoader + 'static,
    A: CustomNodeAdapter + 'static,
{
    pub fn with_loader<Loader>(self, loader: Arc<Loader>) -> Decision<Loader, A>
    where
        Loader: DecisionLoader,
    {
        Decision {
            loader,
            adapter: self.adapter,
            content: self.content,
            validator_cache: self.validator_cache,
        }
    }

    pub fn with_adapter<Adapter>(self, adapter: Arc<Adapter>) -> Decision<L, Adapter>
    where
        Adapter: CustomNodeAdapter,
    {
        Decision {
            loader: self.loader,
            adapter,
            content: self.content,
            validator_cache: self.validator_cache,
        }
    }

    /// Evaluates a decision using an in-memory reference stored in struct
    pub async fn evaluate(
        &self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        self.evaluate_with_opts(context, Default::default()).await
    }

    /// Evaluates a decision using in-memory reference with advanced options
    pub async fn evaluate_with_opts(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let mut decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: options.max_depth.unwrap_or(5),
            trace: options.trace.unwrap_or_default(),
            loader: Arc::new(CachedLoader::from(self.loader.clone())),
            adapter: self.adapter.clone(),
            iteration: 0,
            validator_cache: Some(self.validator_cache.clone()),
        })?;

        let response = decision_graph.evaluate(context).await?;

        Ok(response)
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: 1,
            trace: false,
            loader: Arc::new(CachedLoader::from(self.loader.clone())),
            adapter: self.adapter.clone(),
            iteration: 0,
            validator_cache: Some(self.validator_cache.clone()),
        })?;

        decision_graph.validate()
    }
}

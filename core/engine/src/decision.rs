use std::sync::Arc;

use crate::engine::EvaluationOptions;
use crate::handler::custom_node_adapter::{CustomNodeAdapter, NoopCustomNode};
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::loader::{CachedLoader, DecisionLoader, NoopLoader};
use crate::model::DecisionContent;
use crate::{DecisionGraphValidationError, EvaluationError};
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
}

impl From<DecisionContent> for Decision<NoopLoader, NoopCustomNode> {
    fn from(value: DecisionContent) -> Self {
        Self {
            content: value.into(),
            loader: NoopLoader::default().into(),
            adapter: NoopCustomNode::default().into(),
        }
    }
}

impl From<Arc<DecisionContent>> for Decision<NoopLoader, NoopCustomNode> {
    fn from(value: Arc<DecisionContent>) -> Self {
        Self {
            content: value,
            loader: NoopLoader::default().into(),
            adapter: NoopCustomNode::default().into(),
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
            max_depth: options.max_depth.unwrap_or(5),
            trace: options.trace.unwrap_or_default(),
            loader: Arc::new(CachedLoader::from(self.loader.clone())),
            adapter: self.adapter.clone(),
            iteration: 0,
            content: &self.content,
        })?;

        Ok(decision_graph.evaluate(context).await?)
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            max_depth: 1,
            trace: false,
            loader: Arc::new(CachedLoader::from(self.loader.clone())),
            adapter: self.adapter.clone(),
            iteration: 0,
            content: &self.content,
        })?;

        decision_graph.validate()
    }
}

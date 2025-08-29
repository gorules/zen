use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::engine::{EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind};
use crate::handler::custom_node_adapter::{CustomNodeAdapter, NoopCustomNode};
use crate::loader::{CachedLoader, DecisionLoader, NoopLoader};
use crate::model::DecisionContent;
use crate::nodes::validator_cache::ValidatorCache;
use crate::{DecisionGraphValidationError, EvaluationError};
use serde_json::Value;
use std::sync::Arc;
use zen_expression::variable::Variable;

type DynamicLoader = Arc<dyn DecisionLoader>;
type DynamicCustomNode = Arc<dyn CustomNodeAdapter>;

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<DecisionContent>,
    loader: DynamicLoader,
    adapter: DynamicCustomNode,

    validator_cache: ValidatorCache,
}

impl From<DecisionContent> for Decision {
    fn from(value: DecisionContent) -> Self {
        Self {
            content: value.into(),
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),

            validator_cache: Default::default(),
        }
    }
}

impl From<Arc<DecisionContent>> for Decision {
    fn from(value: Arc<DecisionContent>) -> Self {
        Self {
            content: value,
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),

            validator_cache: Default::default(),
        }
    }
}

impl Decision {
    pub fn with_loader(self, loader: DynamicLoader) -> Self {
        Decision {
            loader,
            adapter: self.adapter,
            content: self.content,
            validator_cache: self.validator_cache,
        }
    }

    pub fn with_adapter(self, adapter: DynamicCustomNode) -> Self {
        Decision {
            adapter,
            loader: self.loader,
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

    pub async fn evaluate_serialized(
        &self,
        context: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value> {
        let response = self
            .evaluate_with_opts(
                context,
                EvaluationOptions {
                    trace: Some(options.trace != EvaluationTraceKind::None),
                    max_depth: options.max_depth,
                },
            )
            .await;

        match response {
            Ok(ok) => Ok(ok
                .serialize_with_mode(serde_json::value::Serializer, options.trace)
                .unwrap_or_default()),
            Err(err) => Err(err
                .serialize_with_mode(serde_json::value::Serializer, options.trace)
                .unwrap_or_default()),
        }
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

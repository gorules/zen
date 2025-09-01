use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::engine::{EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind};
use crate::loader::{DynamicLoader, NoopLoader};
use crate::model::DecisionContent;
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::validator_cache::ValidatorCache;
use crate::nodes::NodeHandlerExtensions;
use crate::{DecisionGraphValidationError, EvaluationError};
use serde_json::Value;
use std::cell::OnceCell;
use std::sync::{Arc, OnceLock};
use zen_expression::variable::Variable;

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<DecisionContent>,
    loader: DynamicLoader,
    adapter: DynamicCustomNode,
    validator_cache: ValidatorCache,
    tokio_runtime: Arc<OnceLock<tokio::runtime::Runtime>>,
}

impl From<DecisionContent> for Decision {
    fn from(value: DecisionContent) -> Self {
        Self {
            content: value.into(),
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            validator_cache: ValidatorCache::default(),
            tokio_runtime: Arc::new(OnceLock::new()),
        }
    }
}

impl From<Arc<DecisionContent>> for Decision {
    fn from(value: Arc<DecisionContent>) -> Self {
        Self {
            content: value,
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            validator_cache: ValidatorCache::default(),
            tokio_runtime: Arc::new(OnceLock::new()),
        }
    }
}

impl Decision {
    pub fn with_loader(mut self, loader: DynamicLoader) -> Self {
        self.loader = loader;
        self
    }

    pub fn with_runtime(mut self, runtime: Arc<OnceLock<tokio::runtime::Runtime>>) -> Self {
        self.tokio_runtime = runtime;
        self
    }

    pub fn with_adapter(mut self, adapter: DynamicCustomNode) -> Self {
        self.adapter = adapter;
        self
    }

    /// Evaluates a decision using an in-memory reference stored in struct
    pub fn evaluate(
        &self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        self.evaluate_with_opts(context, Default::default())
    }

    /// Evaluates a decision using in-memory reference with advanced options
    pub fn evaluate_with_opts(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let mut decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: options.max_depth.unwrap_or(5),
            trace: options.trace.unwrap_or_default(),
            iteration: 0,
            extensions: NodeHandlerExtensions {
                loader: self.loader.clone(),
                custom_node: self.adapter.clone(),
                validator_cache: Arc::new(OnceCell::from(self.validator_cache.clone())),
                tokio_runtime: self.tokio_runtime.clone(),
                ..Default::default()
            },
        })?;

        let response = decision_graph.evaluate(context)?;

        Ok(response)
    }

    pub fn evaluate_serialized(
        &self,
        context: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value> {
        let response = self.evaluate_with_opts(
            context,
            EvaluationOptions {
                trace: Some(options.trace != EvaluationTraceKind::None),
                max_depth: options.max_depth,
            },
        );

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
            iteration: 0,
            extensions: Default::default(),
        })?;

        decision_graph.validate()
    }
}

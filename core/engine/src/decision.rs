use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::engine::{EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind};
use crate::loader::{DynamicLoader, NoopLoader};
use crate::model::GraphContent;
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::function::http_handler::DynamicHttpHandler;
use crate::nodes::NodeHandlerExtensions;
use crate::{DecisionGraphValidationError, EvaluationError};
use serde_json::Value;
use std::cell::OnceCell;
use std::sync::Arc;
use zen_expression::variable::Variable;

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<GraphContent>,
    loader: DynamicLoader,
    adapter: DynamicCustomNode,
    http_handler: DynamicHttpHandler,
}

impl From<GraphContent> for Decision {
    fn from(value: GraphContent) -> Self {
        Self {
            content: value.into(),
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            http_handler: None,
        }
    }
}

impl From<Arc<GraphContent>> for Decision {
    fn from(value: Arc<GraphContent>) -> Self {
        Self {
            content: value,
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            http_handler: None,
        }
    }
}

impl Decision {
    pub fn with_loader(mut self, loader: DynamicLoader) -> Self {
        self.loader = loader;
        self
    }

    pub fn with_adapter(mut self, adapter: DynamicCustomNode) -> Self {
        self.adapter = adapter;
        self
    }

    pub fn with_http_handler(mut self, http_handler: DynamicHttpHandler) -> Self {
        self.http_handler = http_handler;
        self
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
        if let Some(compiled) = self.compiled(&options) {
            let responses = compiled
                .evaluate(
                    &self.content,
                    &self.extensions(),
                    options.max_depth,
                    std::slice::from_ref(&context),
                )
                .await;
            if let Some(response) = responses.into_iter().next() {
                return response;
            }
        }

        self.evaluate_walker(context, options).await
    }

    /// Evaluates a decision for many contexts at once
    pub async fn evaluate_batch(
        &self,
        contexts: &[Variable],
        options: EvaluationOptions,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        if let Some(compiled) = self.compiled(&options) {
            return compiled
                .evaluate(&self.content, &self.extensions(), options.max_depth, contexts)
                .await;
        }

        let mut responses = Vec::with_capacity(contexts.len());
        for context in contexts {
            responses.push(self.evaluate_walker(context.clone(), options).await);
        }
        responses
    }

    pub async fn evaluate_columns<'a>(
        &self,
        columns: &'a zen_expression::lane::Columns<'a>,
        options: EvaluationOptions,
    ) -> crate::compiled::ColumnarOutput<'a> {
        match self.compiled(&options) {
            Some(compiled) => {
                compiled
                    .evaluate_columns(&self.content, &self.extensions(), options.max_depth, columns)
                    .await
            }
            None => {
                let contexts: Vec<Variable> = (0..columns.rows).map(|row| columns.row(row)).collect();
                let results = self.evaluate_batch(&contexts, options).await;
                crate::compiled::ColumnarOutput::from_results(results)
            }
        }
    }

    fn compiled(&self, options: &EvaluationOptions) -> Option<&crate::compiled::CompiledGraph> {
        if options.trace {
            return None;
        }
        self.content.compiled_plan.as_deref()?.usable(&self.content)
    }

    fn extensions(&self) -> NodeHandlerExtensions {
        NodeHandlerExtensions {
            function_runtime: Default::default(),
            loader: self.loader.clone(),
            custom_node: self.adapter.clone(),
            http_handler: self.http_handler.clone(),
            compiled_cache: self.content.compiled_cache.clone(),
            dt_indexes: self.content.dt_indexes.clone(),
            stripped_functions: self.content.stripped_functions.clone(),
            validator_cache: Arc::new(OnceCell::from(self.content.validator_cache.clone())),
        }
    }

    async fn evaluate_walker(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let mut decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: options.max_depth,
            trace: options.trace,
            iteration: 0,
            extensions: self.extensions(),
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
                    trace: options.trace != EvaluationTraceKind::None,
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
            iteration: 0,
            extensions: Default::default(),
        })?;

        decision_graph.validate()
    }

    #[doc(hidden)]
    pub fn interpreted(&self) -> Self {
        let mut content = (*self.content).clone();
        content.compiled_plan = None;
        Self {
            content: Arc::new(content),
            loader: self.loader.clone(),
            adapter: self.adapter.clone(),
            http_handler: self.http_handler.clone(),
        }
    }

    #[doc(hidden)]
    pub fn plan_verdict(&self, columns: &zen_expression::lane::Columns) -> Option<Result<(), String>> {
        let compiled = self.content.compiled_plan.as_deref()?.usable(&self.content)?;
        Some(compiled.plan_verdict(columns))
    }

    pub fn compiled_verdict(&self) -> Option<Result<(), &str>> {
        self.content.compiled_verdict()
    }

    pub fn compile(&mut self) -> () {
        let cm = Arc::make_mut(&mut self.content);
        cm.compile();
    }
}

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
use std::sync::{Arc, OnceLock};
use zen_expression::variable::Variable;

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<GraphContent>,
    prepared: OnceLock<Arc<GraphContent>>,
    hosted: OnceLock<Arc<GraphContent>>,
    interpreted: bool,
    loader: DynamicLoader,
    adapter: DynamicCustomNode,
    http_handler: DynamicHttpHandler,
}

impl From<GraphContent> for Decision {
    fn from(value: GraphContent) -> Self {
        Self {
            content: value.into(),
            prepared: OnceLock::new(),
            hosted: OnceLock::new(),
            interpreted: false,
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
            prepared: OnceLock::new(),
            hosted: OnceLock::new(),
            interpreted: false,
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
        if let Some(compiled) = self.compiled(&options, true) {
            let mut responses = match compiled {
                Ok((compiled, content)) => {
                    let content = compiled.schemas(content, &self.loader).await;
                    compiled
                        .evaluate(&content, &self.extensions(&content), options.max_depth, std::slice::from_ref(&context))
                        .await
                }
                Err(invalid) => vec![Err(invalid.evaluation())],
            };
            if let Some(response) = responses.pop() {
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
        let owned: Vec<Variable> = contexts.iter().map(Variable::deep_clone).collect();
        let contexts = owned.as_slice();
        match self.compiled(&options, false) {
            Some(Ok((compiled, content))) => {
                let content = compiled.schemas(content, &self.loader).await;
                compiled
                    .evaluate(&content, &self.extensions(&content), options.max_depth, contexts)
                    .await
            }
            Some(Err(invalid)) => contexts.iter().map(|_| Err(invalid.evaluation())).collect(),
            None => {
                let mut responses = Vec::with_capacity(contexts.len());
                for context in contexts {
                    responses.push(self.evaluate_walker(context.clone(), options).await);
                }
                responses
            }
        }
    }

    pub async fn evaluate_columns<'a>(
        &self,
        columns: &'a zen_expression::lane::Columns<'a>,
        options: EvaluationOptions,
    ) -> crate::compiled::ColumnarOutput<'a> {
        match self.compiled(&options, false) {
            Some(Ok((compiled, content))) => {
                let content = compiled.schemas(content, &self.loader).await;
                compiled
                    .evaluate_columns(&content, &self.extensions(&content), options.max_depth, columns)
                    .await
            }
            _ => {
                let contexts: Vec<Variable> = (0..columns.rows).map(|row| columns.row(row)).collect();
                let results = self.evaluate_batch(&contexts, options).await;
                crate::compiled::ColumnarOutput::from_results(results)
            }
        }
    }

    fn prepared(&self) -> &Arc<GraphContent> {
        match self.content.compiled_plan.is_some() {
            true => &self.content,
            false => self.prepared.get_or_init(|| {
                let mut owned = (*self.content).clone();
                owned.prepare();
                Arc::new(owned)
            }),
        }
    }

    fn single(&self) -> &Arc<GraphContent> {
        if self.content.compiled_plan.is_some() {
            return &self.content;
        }
        if let Some(prepared) = self.prepared.get() {
            return prepared;
        }
        match self.hosted.get() {
            Some(_) => self.prepared(),
            None => self.hosted.get_or_init(|| Arc::new(self.content.hosted())),
        }
    }

    fn compiled(
        &self,
        options: &EvaluationOptions,
        single: bool,
    ) -> Option<Result<(&crate::compiled::CompiledGraph, &Arc<GraphContent>), &crate::compiled::Invalid>> {
        if options.trace || self.interpreted {
            return None;
        }
        let content = match single {
            true => self.single(),
            false => self.prepared(),
        };
        let plan = content.compiled_plan.as_deref()?;
        Some(plan.graph().map(|compiled| (compiled, content)))
    }

    fn extensions(&self, content: &GraphContent) -> NodeHandlerExtensions {
        NodeHandlerExtensions {
            function_runtime: Default::default(),
            loader: self.loader.clone(),
            custom_node: self.adapter.clone(),
            http_handler: self.http_handler.clone(),
            compiled_cache: content.compiled_cache.clone(),
            dt_indexes: content.dt_indexes.clone(),
            stripped_functions: content.stripped_functions.clone(),
            validator_cache: Arc::new(OnceCell::from(content.validator_cache.clone())),
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
            extensions: self.extensions(&self.content),
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
        Self {
            interpreted: true,
            ..self.clone()
        }
    }

    #[doc(hidden)]
    pub fn plan_verdict(&self, columns: &zen_expression::lane::Columns) -> Option<Result<(), String>> {
        let compiled = self.prepared().compiled_plan.as_deref()?.graph().ok()?;
        Some(compiled.plan_verdict(columns))
    }

    pub fn compiled_verdict(&self) -> Option<Result<(), &str>> {
        self.content.compiled_verdict()
    }

    pub fn compile(&mut self) -> () {
        let cm = Arc::make_mut(&mut self.content);
        cm.compile();
        self.prepared = OnceLock::new();
        self.hosted = OnceLock::new();
    }
}

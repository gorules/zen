use crate::compiled::traced::Traced;
use crate::compiled::{CompiledGraph, CompiledPlan, Invalid};
use crate::decision_graph::graph::{DecisionGraph, DecisionGraphResponse};
#[cfg(feature = "driver")]
use crate::decision_graph::walk::DecisionGraphConfig;
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

type Planned = (Arc<GraphContent>, Arc<CompiledPlan>);

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<GraphContent>,
    prepared: OnceLock<Planned>,
    hosted: OnceLock<Planned>,
    #[cfg(feature = "driver")]
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
            #[cfg(feature = "driver")]
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
            #[cfg(feature = "driver")]
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
        #[cfg(feature = "driver")]
        if self.interpreted {
            return self.evaluate_walker(context, options).await;
        }
        if options.trace {
            return self.traced(context, options).await;
        }

        let (compiled, content) = Self::compiled(self.single()).map_err(Invalid::evaluation)?;
        let content = compiled.schemas(content, &self.loader).await;
        compiled
            .evaluate_one(&content, &self.extensions(&content), options.max_depth, context)
            .await
    }

    /// Evaluates a decision for many contexts at once
    pub async fn evaluate_batch(
        &self,
        contexts: &[Variable],
        options: EvaluationOptions,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let owned: Vec<Variable> = contexts.iter().map(Variable::deep_clone).collect();
        if self.walked() || options.trace {
            let mut responses = Vec::with_capacity(owned.len());
            for context in owned {
                responses.push(self.evaluate_with_opts(context, options).await);
            }
            return responses;
        }

        match Self::compiled(self.prepared()) {
            Ok((compiled, content)) => {
                let content = compiled.schemas(content, &self.loader).await;
                compiled
                    .evaluate(&content, &self.extensions(&content), options.max_depth, &owned)
                    .await
            }
            Err(invalid) => owned.iter().map(|_| Err(invalid.evaluation())).collect(),
        }
    }

    pub async fn evaluate_columns<'a>(
        &self,
        columns: &'a zen_expression::lane::Columns<'a>,
        options: EvaluationOptions,
    ) -> crate::compiled::ColumnarOutput<'a> {
        match (self.walked(), Self::compiled(self.prepared())) {
            (false, Ok((compiled, content))) => {
                let content = compiled.schemas(content, &self.loader).await;
                compiled
                    .evaluate_columns(&content, &self.extensions(&content), options.max_depth, columns)
                    .await
            }
            _ => {
                let contexts: Vec<Variable> = (0..columns.rows).map(|row| columns.row(row)).collect();
                let options = EvaluationOptions {
                    trace: false,
                    ..options
                };
                let results = self.evaluate_batch(&contexts, options).await;
                crate::compiled::ColumnarOutput::from_results(results)
            }
        }
    }

    async fn traced(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let (compiled, content) = Self::compiled(self.tracing()).map_err(Invalid::evaluation)?;
        let content = compiled.schemas(content, &self.loader).await;
        let extensions = self.extensions(&content);
        let traced = Traced {
            graph: compiled,
            content: &content,
            extensions: &extensions,
            max_depth: options.max_depth,
            iteration: 0,
            parent: None,
        };
        traced.evaluate(context).await
    }

    fn prepared(&self) -> (&Arc<GraphContent>, &CompiledPlan) {
        if let Some(plan) = self.content.compiled_plan.as_deref() {
            return (&self.content, plan);
        }
        let (content, plan) = self.prepared.get_or_init(|| {
            let mut owned = (*self.content).clone();
            let plan = owned.prepare();
            (Arc::new(owned), plan)
        });
        (content, plan)
    }

    fn hosted(&self) -> (&Arc<GraphContent>, &CompiledPlan) {
        let (content, plan) = self.hosted.get_or_init(|| {
            let (content, plan) = self.content.hosted();
            (Arc::new(content), plan)
        });
        (content, plan)
    }

    fn single(&self) -> (&Arc<GraphContent>, &CompiledPlan) {
        match (self.prepared.get(), self.hosted.get()) {
            (None, None) if self.content.compiled_plan.is_none() => self.hosted(),
            _ => self.prepared(),
        }
    }

    fn tracing(&self) -> (&Arc<GraphContent>, &CompiledPlan) {
        match self.prepared.get() {
            None if self.content.compiled_plan.is_none() => self.hosted(),
            _ => self.prepared(),
        }
    }

    fn compiled<'c>(
        (content, plan): (&'c Arc<GraphContent>, &'c CompiledPlan),
    ) -> Result<(&'c CompiledGraph, &'c Arc<GraphContent>), &'c Invalid> {
        plan.graph().map(|compiled| (compiled, content))
    }

    fn extensions(&self, content: &GraphContent) -> NodeHandlerExtensions {
        NodeHandlerExtensions {
            function_runtime: Default::default(),
            loader: self.loader.clone(),
            custom_node: self.adapter.clone(),
            http_handler: self.http_handler.clone(),
            #[cfg(feature = "driver")]
            compiled_cache: content.compiled_cache.clone(),
            dt_indexes: content.dt_indexes.clone(),
            stripped_functions: content.stripped_functions.clone(),
            validator_cache: Arc::new(OnceCell::from(content.validator_cache.clone())),
        }
    }

    #[cfg(feature = "driver")]
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
        DecisionGraph::validate_graph(&DecisionGraph::build_graph(&self.content)?)
    }

    fn walked(&self) -> bool {
        #[cfg(feature = "driver")]
        if self.interpreted {
            return true;
        }
        false
    }

    #[cfg(feature = "driver")]
    #[doc(hidden)]
    pub fn interpreted(&self) -> Self {
        Self {
            interpreted: true,
            ..self.clone()
        }
    }

    #[doc(hidden)]
    pub fn plan_verdict(&self, columns: &zen_expression::lane::Columns) -> Option<Result<(), String>> {
        let (compiled, _) = Self::compiled(self.prepared()).ok()?;
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

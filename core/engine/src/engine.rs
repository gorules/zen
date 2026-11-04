use crate::decision::Decision;
use crate::decision_graph::graph::DecisionGraphResponse;
use crate::error::ContentKindError;
use crate::loader::{ClosureLoader, DynamicLoader, LoaderResponse, LoaderResult, NoopLoader};
use crate::model::{DecisionContent, GraphContent};
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::function::http_handler::DynamicHttpHandler;
use crate::policy::runtime::{CompiledEntry, CompiledSet};
use crate::trace::TraceFormat;
use crate::{CompileFailure, EvaluationError};
use arc_swap::ArcSwapOption;
use serde_json::Value;
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;
use zen_expression::variable::Variable;

/// Structure used for generating and evaluating JDM decisions
#[derive(Clone)]
pub struct DecisionEngine {
    loader: DynamicLoader,
    adapter: DynamicCustomNode,
    http_handler: DynamicHttpHandler,
    compiled: Arc<ArcSwapOption<CompiledSet>>,
    caches: Arc<crate::compiled::engine::Caches>,
    engine: crate::compiled::Engine,
}

impl Debug for DecisionEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionEngine")
            .field("loader", &self.loader)
            .field("adapter", &self.adapter)
            .field("http_handler", &self.http_handler)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EvaluationOptions {
    pub trace: TraceFormat,
    pub max_depth: u8,
    pub now: Option<zen_expression::clock::Instant>,
    pub timezone: Option<zen_expression::clock::Timezone>,
    pub seed: Option<u64>,
}

impl Default for EvaluationOptions {
    fn default() -> Self {
        Self {
            trace: TraceFormat::Off,
            max_depth: 10,
            now: None,
            timezone: None,
            seed: None,
        }
    }
}

pub type EvaluationSerializedOptions = EvaluationOptions;

impl Default for DecisionEngine {
    fn default() -> Self {
        Self {
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            http_handler: None,
            compiled: Arc::new(ArcSwapOption::empty()),
            caches: Default::default(),
            engine: crate::compiled::Engine::Configured,
        }
    }
}

impl DecisionEngine {
    pub fn new(loader: DynamicLoader, adapter: DynamicCustomNode) -> Self {
        Self {
            loader,
            adapter,
            http_handler: None,
            compiled: Arc::new(ArcSwapOption::empty()),
            caches: Default::default(),
            engine: crate::compiled::Engine::Configured,
        }
    }

    pub fn with_adapter(mut self, adapter: DynamicCustomNode) -> Self {
        self.adapter = adapter;
        self.compiled = Arc::new(ArcSwapOption::empty());
        self.caches = Default::default();
        self
    }

    pub fn with_loader(mut self, loader: DynamicLoader) -> Self {
        self.loader = loader;
        self.compiled = Arc::new(ArcSwapOption::empty());
        self.caches = Default::default();
        self
    }

    pub fn with_http_handler(mut self, http_handler: DynamicHttpHandler) -> Self {
        self.http_handler = http_handler;
        self.compiled = Arc::new(ArcSwapOption::empty());
        self.caches = Default::default();
        self
    }

    pub fn with_closure_loader<F, O>(mut self, loader: F) -> Self
    where
        F: Fn(String) -> O + Sync + Send + 'static,
        O: Future<Output = LoaderResponse> + Send,
    {
        self.loader = Arc::new(ClosureLoader::new(loader));
        self.compiled = Arc::new(ArcSwapOption::empty());
        self.caches = Default::default();
        self
    }

    pub fn compile(&self) -> Vec<CompileFailure> {
        let Some(keys) = self.loader.keys() else {
            return Vec::new();
        };

        let set = CompiledSet::build_sync(&self.loader, &keys);
        if self.engine.compiled() {
            self.caches.compile(&self.loader, &keys);
        }

        let failures = set.failures().to_vec();
        self.compiled.store(Some(Arc::new(set)));
        failures
    }

    pub fn compile_failures(&self) -> Vec<CompileFailure> {
        self.compiled
            .load_full()
            .map(|set| set.failures().to_vec())
            .unwrap_or_default()
    }

    /// Evaluates a decision through loader using a key
    pub async fn evaluate<K>(
        &self,
        key: K,
        context: Variable,
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
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>>
    where
        K: AsRef<str>,
    {
        let key_str = key.as_ref();
        if self.engine.compiled() {
            return self
                .caches
                .evaluate_pinned(&self.parts(), key_str, context, options)
                .await?;
        }
        if self.engine.scopes() {
            return crate::compiled::Engine::interpreting(
                self.evaluate_with_opts(key_str, context, options),
            )
            .await;
        }
        if let Some(set) = self.compiled.load_full() {
            if let Some(entry) = set.get(key_str) {
                return match entry {
                    CompiledEntry::Policy(artifact) => artifact
                        .evaluate_entry(key_str, context, options.trace.is_on())
                        .map(DecisionGraphResponse::from)
                        .map_err(|e| Box::new(EvaluationError::Policy(e.into()))),
                    CompiledEntry::Graph(graph) => {
                        self.decision_from_graph(graph)
                            .evaluate_with_opts(context, options)
                            .await
                    }
                };
            }
        }
        let content = self.loader.load(key_str).await?;
        match content.as_ref() {
            DecisionContent::Graph(_) => {
                let decision = self.decision_from_graph_arc(content);
                decision.evaluate_with_opts(context, options).await
            }
            DecisionContent::Policy(_) => {
                crate::policy::runtime::evaluate_policy(
                    &self.loader,
                    key_str,
                    content,
                    context,
                    options,
                )
                .await
            }
        }
    }

    pub async fn evaluate_serialized<K>(
        &self,
        key: K,
        context: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value>
    where
        K: AsRef<str>,
    {
        let key_str = key.as_ref();
        if self.engine.compiled() {
            return self
                .caches
                .evaluate_serialized_pinned(&self.parts(), key_str, context, options)
                .await;
        }
        if self.engine.scopes() {
            return crate::compiled::Engine::interpreting(
                self.evaluate_serialized(key_str, context, options),
            )
            .await;
        }
        if let Some(set) = self.compiled.load_full() {
            if let Some(entry) = set.get(key_str) {
                match entry {
                    CompiledEntry::Policy(artifact) => {
                        return DecisionGraphResponse::serialized(
                            artifact
                                .evaluate_entry(key_str, context, options.trace.is_on())
                                .map(DecisionGraphResponse::from)
                                .map_err(|e| Box::new(EvaluationError::Policy(e.into()))),
                        );
                    }
                    CompiledEntry::Graph(graph) => {
                        return self
                            .decision_from_graph(graph)
                            .evaluate_serialized(context, options)
                            .await;
                    }
                }
            }
        }
        let content = self
            .loader
            .load(key_str)
            .await
            .map_err(|err| Value::String(err.to_string()))?;

        match content.as_ref() {
            DecisionContent::Graph(_) => {
                let decision = self.decision_from_graph_arc(content);
                decision.evaluate_serialized(context, options).await
            }
            DecisionContent::Policy(_) => DecisionGraphResponse::serialized(
                crate::policy::runtime::evaluate_policy(
                    &self.loader,
                    key_str,
                    content,
                    context,
                    options,
                )
                .await,
            ),
        }
    }

    pub async fn evaluate_batch<K>(
        &self,
        key: K,
        contexts: &[Variable],
        options: EvaluationOptions,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>>
    where
        K: AsRef<str>,
    {
        self.caches
            .evaluate_batch_pinned(&self.parts(), key.as_ref(), contexts, options)
            .await
    }

    #[doc(hidden)]
    pub fn interpreted(mut self) -> Self {
        self.engine = crate::compiled::Engine::Interpreted;
        self
    }

    #[doc(hidden)]
    pub fn compiled_engine(mut self) -> Self {
        self.engine = crate::compiled::Engine::Compiled;
        self
    }

    fn parts(&self) -> crate::compiled::engine::Parts<'_> {
        crate::compiled::engine::Parts {
            loader: &self.loader,
            adapter: &self.adapter,
            http_handler: &self.http_handler,
        }
    }

    fn decision_from_graph_arc(&self, content: Arc<DecisionContent>) -> Decision {
        let graph: Arc<GraphContent> = match Arc::try_unwrap(content) {
            Ok(DecisionContent::Graph(g)) => g,
            Err(arc) => match arc.as_ref() {
                DecisionContent::Graph(g) => g.clone(),
                DecisionContent::Policy(_) => {
                    panic!("decision_from_graph_arc called with Policy variant")
                }
            },
            Ok(DecisionContent::Policy(_)) => {
                panic!("decision_from_graph_arc called with Policy variant")
            }
        };
        self.decision_from_graph(graph)
    }

    fn decision_from_graph(&self, graph: Arc<GraphContent>) -> Decision {
        Decision::from(graph)
            .with_loader(self.loader.clone())
            .with_adapter(self.adapter.clone())
            .with_http_handler(self.http_handler.clone())
            .with_engine(self.engine)
    }

    /// Creates a decision from DecisionContent, exists for easier binding creation
    pub fn create_decision(
        &self,
        content: Arc<DecisionContent>,
    ) -> Result<Decision, ContentKindError> {
        match content.as_ref() {
            DecisionContent::Graph(_) => Ok(self.decision_from_graph_arc(content)),
            DecisionContent::Policy(_) => Err(ContentKindError {
                expected: "graph",
                got: "policy",
            }),
        }
    }

    /// Retrieves a decision based on the loader
    pub async fn get_decision(
        &self,
        key: &str,
    ) -> LoaderResult<Result<Decision, ContentKindError>> {
        let content = self.loader.load(key).await?;
        Ok(self.create_decision(content))
    }
    pub fn loader(&self) -> DynamicLoader {
        self.loader.clone()
    }
}

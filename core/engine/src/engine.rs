use crate::decision::Decision;
use crate::decision_graph::graph::{DecisionGraphResponse, EvaluationTrace};
use crate::error::ContentKindError;
use crate::loader::{ClosureLoader, DynamicLoader, LoaderResponse, LoaderResult, NoopLoader};
use crate::model::{DecisionContent, GraphContent};
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::function::http_handler::DynamicHttpHandler;
use crate::policy::runtime::{CompiledEntry, CompiledSet};
use crate::{CompileFailure, EvaluationError};
use arc_swap::ArcSwapOption;
use serde_json::Value;
use std::fmt::Debug;
use std::future::Future;
use std::sync::Arc;
use strum::{EnumString, IntoStaticStr};
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

/// Options for one evaluation.
///
/// `now`, `timezone`, `seed` and `locale` pin the evaluation context of the compiled engine:
/// `d()`, `rand()` and the date helpers in expressions, and `Date`, `Math.random`, local time,
/// `Intl` and `toLocale*` in function nodes read them instead of the host clock, zone and
/// random source. Each one left `None` is resolved lazily: `now` from the host clock once per
/// evaluation (once per batch), `timezone` from the host zone, `seed` at random on the first
/// draw and `locale` to `en-US`. In a batch, row `i` draws from its own stream seeded with
/// `zen_engine::context::row_seed(seed, i)`, so it replays alone with that seed.
///
/// The interpreted engine (`compiled: false`) ignores these four fields and keeps using the host
/// clock, the host zone and an unseeded random source.
///
/// Outbound HTTP from function nodes is not captured by the context, so replaying a graph that
/// makes such calls is not guaranteed to reproduce its result.
#[derive(Debug, Clone, Copy)]
pub struct EvaluationOptions {
    pub trace: bool,
    pub max_depth: u8,
    /// Instant the evaluation runs at.
    pub now: Option<zen_expression::context::Instant>,
    /// IANA time zone for local dates, e.g. `Europe/Berlin`.
    pub timezone: Option<zen_expression::context::Timezone>,
    /// Seed of the random streams.
    pub seed: Option<u64>,
    /// BCP 47 locale for function nodes, `en-US` when `None`.
    pub locale: Option<zen_expression::context::Locale>,
}

impl Default for EvaluationOptions {
    fn default() -> Self {
        Self {
            trace: false,
            max_depth: 10,
            now: None,
            timezone: None,
            seed: None,
            locale: None,
        }
    }
}

/// [`EvaluationOptions`] with a serialized trace; the context fields behave the same.
#[derive(Debug, Clone, Copy)]
pub struct EvaluationSerializedOptions {
    pub trace: EvaluationTraceKind,
    pub max_depth: u8,
    /// Instant the evaluation runs at.
    pub now: Option<zen_expression::context::Instant>,
    /// IANA time zone for local dates, e.g. `Europe/Berlin`.
    pub timezone: Option<zen_expression::context::Timezone>,
    /// Seed of the random streams.
    pub seed: Option<u64>,
    /// BCP 47 locale for function nodes, `en-US` when `None`.
    pub locale: Option<zen_expression::context::Locale>,
}

impl Default for EvaluationSerializedOptions {
    fn default() -> Self {
        Self {
            trace: EvaluationTraceKind::None,
            max_depth: 10,
            now: None,
            timezone: None,
            seed: None,
            locale: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum EvaluationTraceKind {
    #[default]
    None,
    Default,
    String,
    Reference,
    ReferenceString,
}

impl EvaluationTraceKind {
    pub fn serialize_trace(&self, trace: &Variable) -> Value {
        match self {
            EvaluationTraceKind::None => Value::Null,
            EvaluationTraceKind::Default => serde_json::to_value(&trace).unwrap_or_default(),
            EvaluationTraceKind::String => {
                Value::String(serde_json::to_string(&trace).unwrap_or_default())
            }
            EvaluationTraceKind::Reference => {
                serde_json::to_value(&trace.serialize_ref()).unwrap_or_default()
            }
            EvaluationTraceKind::ReferenceString => {
                Value::String(serde_json::to_string(&trace.serialize_ref()).unwrap_or_default())
            }
        }
    }
}

impl EvaluationOptions {
    pub(crate) fn pinned(&self) -> zen_expression::context::Pinned {
        zen_expression::context::Pinned {
            now: self.now,
            timezone: self.timezone,
            seed: self.seed,
            locale: self.locale,
        }
    }
}

impl EvaluationSerializedOptions {
    pub(crate) fn pinned(&self) -> zen_expression::context::Pinned {
        self.unserialized().pinned()
    }

    pub(crate) fn unserialized(&self) -> EvaluationOptions {
        EvaluationOptions {
            trace: self.trace != EvaluationTraceKind::None,
            max_depth: self.max_depth,
            now: self.now,
            timezone: self.timezone,
            seed: self.seed,
            locale: self.locale,
        }
    }
}

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
                        .evaluate_entry(key_str, context, options.trace)
                        .map(|r| DecisionGraphResponse {
                            performance: format!("{:.1?}", r.duration),
                            result: r.output,
                            trace: r.trace.map(EvaluationTrace::Policy),
                        })
                        .map_err(|e| Box::new(EvaluationError::Policy(e))),
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
                        let trace_mode = options.trace;
                        let trace = options.trace != EvaluationTraceKind::None;
                        return match artifact.evaluate_entry(key_str, context, trace) {
                            Ok(r) => {
                                let response = DecisionGraphResponse {
                                    performance: format!("{:.1?}", r.duration),
                                    result: r.output,
                                    trace: r.trace.map(EvaluationTrace::Policy),
                                };
                                Ok(response
                                    .serialize_with_mode(serde_json::value::Serializer, trace_mode)
                                    .unwrap_or_default())
                            }
                            Err(e) => {
                                let err = EvaluationError::Policy(e);
                                Err(err
                                    .serialize_with_mode(serde_json::value::Serializer, trace_mode)
                                    .unwrap_or_default())
                            }
                        };
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
            DecisionContent::Policy(_) => {
                let inner_opts = options.unserialized();
                let trace_mode = options.trace;
                let response = crate::policy::runtime::evaluate_policy(
                    &self.loader,
                    key_str,
                    content,
                    context,
                    inner_opts,
                )
                .await;
                match response {
                    Ok(ok) => Ok(ok
                        .serialize_with_mode(serde_json::value::Serializer, trace_mode)
                        .unwrap_or_default()),
                    Err(err) => Err(err
                        .serialize_with_mode(serde_json::value::Serializer, trace_mode)
                        .unwrap_or_default()),
                }
            }
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

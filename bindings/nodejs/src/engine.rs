use std::str::FromStr;
use std::sync::Arc;

use napi::anyhow::{anyhow, Context};
use napi::bindgen_prelude::{
    Buffer, Either, Either3, FromNapiValue, Function, Object, Promise, ToNapiValue,
};
use napi::sys::{napi_env, napi_value};
use napi::{Env, JsValue, Unknown, ValueType};
use napi_derive::napi;
use serde_json::Value;

use crate::content::ZenDecisionContent;
use crate::convert::NodeEvalResponse;
use crate::custom_node::{CustomNode, CustomNodeTsfn};
use crate::decision::ZenDecision;
use crate::dispose::DisposeThreadsafeHandler;
use crate::http_handler::{
    HttpHandlerTsfn, NodeHttpHandler, ZenHttpHandlerRequest, ZenHttpHandlerResponse,
};
use crate::loader::{DecisionLoader, LoaderTsfn};
use crate::mt::spawn_worker;
use crate::safe_result::SafeResult;
use crate::types::{ZenEngineHandlerRequest, ZenEngineHandlerResponse};
use zen_engine::loader::{DynamicLoader, LoaderConfig};
use zen_engine::model::DecisionContent;
use zen_engine::{
    DecisionEngine, EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind,
};

#[napi]
pub struct ZenEngine {
    graph: Arc<DecisionEngine>,

    custom_node_tsfn: Option<CustomNodeTsfn>,
    loader_tsfn: Option<LoaderTsfn>,
    http_handler_tsfn: Option<HttpHandlerTsfn>,
}

#[derive(Debug, Default)]
pub struct JsEvaluationTraceKind(pub EvaluationTraceKind);

impl FromNapiValue for JsEvaluationTraceKind {
    unsafe fn from_napi_value(env: napi_env, napi_val: napi_value) -> napi::Result<Self> {
        let js_value = Unknown::from_napi_value(env, napi_val)?;

        match js_value.get_type()? {
            ValueType::Undefined | ValueType::Null => Ok(JsEvaluationTraceKind::default()),
            ValueType::Boolean => {
                let enabled = js_value.coerce_to_bool()?;
                let kind = match enabled {
                    true => EvaluationTraceKind::Default,
                    false => EvaluationTraceKind::None,
                };

                Ok(JsEvaluationTraceKind(kind))
            }
            ValueType::String => {
                let kind_utf8 = js_value.coerce_to_string()?.into_utf8()?;
                let kind_str = kind_utf8.as_str()?;
                let kind =
                    EvaluationTraceKind::from_str(kind_str).context("invalid evaluation mode")?;

                Ok(JsEvaluationTraceKind(kind))
            }
            _ => Err(anyhow!("Invalid trace setting").into()),
        }
    }
}

impl ToNapiValue for JsEvaluationTraceKind {
    unsafe fn to_napi_value(env: napi_env, val: Self) -> napi::Result<napi_value> {
        match val.0 {
            EvaluationTraceKind::None => ToNapiValue::to_napi_value(env, false),
            EvaluationTraceKind::Default => ToNapiValue::to_napi_value(env, true),
            _ => {
                let mode_str: &'static str = val.0.into();
                ToNapiValue::to_napi_value(env, mode_str)
            }
        }
    }
}

#[derive(Debug)]
#[napi(object)]
pub struct ZenEvaluateOptions {
    pub max_depth: Option<u8>,
    #[napi(ts_type = "boolean | 'string' | 'reference' | 'referenceString'")]
    pub trace: Option<JsEvaluationTraceKind>,
}

impl Default for ZenEvaluateOptions {
    fn default() -> Self {
        Self {
            max_depth: Some(5),
            trace: Some(JsEvaluationTraceKind::default()),
        }
    }
}

impl From<ZenEvaluateOptions> for EvaluationSerializedOptions {
    fn from(value: ZenEvaluateOptions) -> Self {
        Self {
            max_depth: value.max_depth.unwrap_or(5),
            trace: value.trace.unwrap_or_default().0,
        }
    }
}

#[napi(object)]
pub struct EvaluateBatchRequest {
    pub key: String,
    pub context: Value,
}

#[napi(object)]
pub struct EvaluateBatchRawRequest {
    pub key: String,
    pub context: Buffer,
}

pub struct EvaluateBatchRawResult {
    pub success: bool,
    pub data: Option<Buffer>,
    pub error: Option<Value>,
}

impl ToNapiValue for EvaluateBatchRawResult {
    unsafe fn to_napi_value(env: napi_env, val: Self) -> napi::Result<napi_value> {
        let env_wrapper = &Env::from(env);
        let mut obj = Object::new(env_wrapper)?;
        obj.set("success", val.success)?;
        obj.set("data", val.data)?;
        obj.set("error", val.error)?;
        Object::to_napi_value(env, obj)
    }
}

pub struct EvaluateBatchResult {
    pub success: bool,
    pub data: Option<NodeEvalResponse>,
    pub error: Option<Value>,
}

impl ToNapiValue for EvaluateBatchResult {
    unsafe fn to_napi_value(env: napi_env, val: Self) -> napi::Result<napi_value> {
        let env_wrapper = &Env::from(env);
        let mut obj = Object::new(env_wrapper)?;
        obj.set("success", val.success)?;
        obj.set("data", val.data)?;
        obj.set("error", val.error)?;
        Object::to_napi_value(env, obj)
    }
}

#[napi(object)]
pub struct ZenEngineOptions {
    #[napi(
        ts_type = "((key: string) => Promise<Buffer | ZenDecisionContent>) | { type: 'static'; content: Record<string, object> } | { type: 'fs'; path: string } | { type: 'zip'; bytes: Buffer }"
    )]
    pub loader: Option<
        Either<
            Function<'static, String, Promise<Option<Either<Buffer, &'static ZenDecisionContent>>>>,
            Object<'static>,
        >,
    >,

    #[napi(ts_type = "(request: ZenEngineHandlerRequest) => Promise<ZenEngineHandlerResponse>")]
    pub custom_handler:
        Option<Function<'static, ZenEngineHandlerRequest, Promise<ZenEngineHandlerResponse>>>,

    #[napi(ts_type = "(request: ZenHttpHandlerRequest) => Promise<ZenHttpHandlerResponse>")]
    pub http_handler:
        Option<Function<'static, ZenHttpHandlerRequest, Promise<ZenHttpHandlerResponse>>>,
}

#[napi]
impl ZenEngine {
    #[napi(constructor)]
    pub fn new(env: Env, options: Option<ZenEngineOptions>) -> napi::Result<Self> {
        let Some(opts) = options else {
            return Ok(Self {
                graph: DecisionEngine::new(
                    Arc::new(DecisionLoader::default()),
                    Arc::new(CustomNode::default()),
                )
                .into(),

                loader_tsfn: None,
                http_handler_tsfn: None,
                custom_node_tsfn: None,
            });
        };

        let mut loader_tsfn_opt: Option<LoaderTsfn> = None;
        let mut http_handler_tsfn_opt: Option<HttpHandlerTsfn> = None;
        let mut custom_node_tsfn_opt: Option<CustomNodeTsfn> = None;

        let loader: DynamicLoader = match opts.loader {
            None => Arc::new(DecisionLoader::default()),
            Some(Either::A(func)) => {
                let loader_tsfn = func
                    .build_threadsafe_function()
                    .max_queue_size::<0>()
                    .callee_handled::<false>()
                    .weak()
                    .build()?;

                let arc_loader_tsfn = Arc::new(loader_tsfn);
                loader_tsfn_opt = Some(arc_loader_tsfn.clone());
                Arc::new(DecisionLoader::new(arc_loader_tsfn))
            }
            Some(Either::B(config_obj)) => {
                let loader_type: Option<String> = config_obj.get("type")?;
                let config: LoaderConfig = match loader_type.as_deref() {
                    Some("zip") => {
                        let bytes: Buffer = config_obj
                            .get("bytes")?
                            .ok_or_else(|| anyhow!("zip loader requires a 'bytes' buffer"))?;
                        LoaderConfig::Zip {
                            bytes: bytes.to_vec(),
                        }
                    }
                    _ => env.from_js_value(config_obj)?,
                };

                config.into_loader().map_err(|e| anyhow!(e))?
            }
        };

        let custom_node = match opts.custom_handler {
            None => CustomNode::default(),
            Some(c) => {
                let custom_tfsn = c
                    .build_threadsafe_function()
                    .max_queue_size::<0>()
                    .callee_handled::<false>()
                    .weak()
                    .build()?;

                let arc_custom_node = Arc::new(custom_tfsn);
                custom_node_tsfn_opt = Some(arc_custom_node.clone());
                CustomNode::new(arc_custom_node)
            }
        };

        let mut decision_engine = DecisionEngine::new(loader, Arc::new(custom_node));
        if let Some(h) = opts.http_handler {
            let http_tsfn = h
                .build_threadsafe_function()
                .max_queue_size::<0>()
                .callee_handled::<false>()
                .weak()
                .build()?;

            let arc_http_handler_tsfn = Arc::new(http_tsfn);
            http_handler_tsfn_opt = Some(arc_http_handler_tsfn.clone());
            decision_engine = decision_engine
                .with_http_handler(Some(Arc::new(NodeHttpHandler::new(arc_http_handler_tsfn))));
        }

        decision_engine.compile();

        Ok(Self {
            graph: Arc::new(decision_engine),

            loader_tsfn: loader_tsfn_opt,
            http_handler_tsfn: http_handler_tsfn_opt,
            custom_node_tsfn: custom_node_tsfn_opt,
        })
    }

    #[napi(ts_return_type = "Promise<ZenEngineResponse>")]
    pub async fn evaluate(
        &self,
        key: String,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
    ) -> napi::Result<NodeEvalResponse> {
        let graph = self.graph.clone();
        let result = spawn_worker(|| {
            let serialized: EvaluationSerializedOptions = opts.unwrap_or_default().into();
            let mode = serialized.trace;
            let options = EvaluationOptions {
                trace: mode != EvaluationTraceKind::None,
                max_depth: serialized.max_depth,
            };

            async move {
                let context = zen_engine::Variable::try_from_value(context).map_err(
                    |e| serde_json::json!({ "type": "ContextError", "source": e.to_string() }),
                )?;
                graph
                    .evaluate_with_opts(key, context, options)
                    .await
                    .map(|response| NodeEvalResponse::build(response, mode))
                    .map_err(|e| {
                        e.serialize_with_mode(serde_json::value::Serializer, mode)
                            .unwrap_or_default()
                    })
            }
        })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
        .map_err(|e| anyhow!(e))?;

        Ok(result)
    }

    #[napi]
    pub fn create_decision(
        &self,
        env: Env,
        content: Either3<&ZenDecisionContent, Buffer, Object>,
    ) -> napi::Result<ZenDecision> {
        let decision_content: Arc<DecisionContent> = match content {
            Either3::A(c) => c.inner.clone(),
            Either3::B(buffer) => Arc::new(serde_json::from_slice(buffer.as_ref())?),
            Either3::C(obj) => {
                let serde_val: Value = env.from_js_value(obj)?;
                Arc::new(serde_json::from_value(serde_val)?)
            }
        };

        let decision = self
            .graph
            .create_decision(decision_content)
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(ZenDecision::from(decision))
    }

    #[napi]
    pub async fn get_decision(&self, key: String) -> napi::Result<ZenDecision> {
        let decision = self
            .graph
            .get_decision(&key)
            .await
            .with_context(|| format!("Failed to find decision with key = {key}"))?
            .map_err(|e| anyhow!(e.to_string()))?;

        // TODO: Investigate why reference leak?
        Ok(ZenDecision::from(decision))
    }

    #[napi(
        ts_return_type = "Promise<{ success: true, data: ZenEngineResponse } | { success: false; error: any; }>"
    )]
    pub async fn safe_evaluate(
        &self,
        key: String,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
    ) -> SafeResult<NodeEvalResponse> {
        self.evaluate(key, context, opts).await.into()
    }

    #[napi(
        ts_return_type = "Promise<{ success: true, data: ZenDecision } | { success: false; error: any; }>"
    )]
    pub async fn safe_get_decision(&self, key: String) -> SafeResult<ZenDecision> {
        self.get_decision(key).await.into()
    }

    #[napi(
        ts_return_type = "Promise<Array<{ success: true; data: ZenEngineResponse } | { success: false; error: any }>>"
    )]
    pub async fn evaluate_batch(
        &self,
        requests: Vec<EvaluateBatchRequest>,
        opts: Option<ZenEvaluateOptions>,
    ) -> napi::Result<Vec<EvaluateBatchResult>> {
        let options: EvaluationSerializedOptions = opts.unwrap_or_default().into();
        let mode = options.trace;
        let max_depth = options.max_depth;

        let mut handles = Vec::with_capacity(requests.len());
        for req in requests {
            let engine = self.graph.clone();
            let EvaluateBatchRequest { key, context } = req;
            handles.push(spawn_worker(move || async move {
                let eval_opts = EvaluationOptions {
                    trace: mode != EvaluationTraceKind::None,
                    max_depth,
                };

                let context = zen_engine::Variable::try_from_value(context).map_err(
                    |e| serde_json::json!({ "type": "ContextError", "source": e.to_string() }),
                )?;
                engine
                    .evaluate_with_opts(key, context, eval_opts)
                    .await
                    .map(|response| NodeEvalResponse::build(response, mode))
                    .map_err(|e| {
                        e.serialize_with_mode(serde_json::value::Serializer, mode)
                            .unwrap_or_default()
                    })
            }));
        }

        let mut out = Vec::with_capacity(handles.len());
        for handle in handles {
            out.push(match handle.await {
                Ok(Ok(data)) => EvaluateBatchResult {
                    success: true,
                    data: Some(data),
                    error: None,
                },
                Ok(Err(error)) => EvaluateBatchResult {
                    success: false,
                    data: None,
                    error: Some(error),
                },
                Err(_) => EvaluateBatchResult {
                    success: false,
                    data: None,
                    error: Some(Value::String("evaluation worker panicked".into())),
                },
            });
        }

        Ok(out)
    }

    #[napi(
        ts_return_type = "Promise<Array<{ success: true; data: Buffer } | { success: false; error: any }>>"
    )]
    pub async fn evaluate_batch_raw(
        &self,
        requests: Vec<EvaluateBatchRawRequest>,
        opts: Option<ZenEvaluateOptions>,
    ) -> napi::Result<Vec<EvaluateBatchRawResult>> {
        let options: EvaluationSerializedOptions = opts.unwrap_or_default().into();
        let mode = options.trace;
        let max_depth = options.max_depth;

        let mut handles = Vec::with_capacity(requests.len());
        for req in requests {
            let engine = self.graph.clone();
            let EvaluateBatchRawRequest { key, context } = req;
            let bytes = context.to_vec();
            handles.push(spawn_worker(move || async move {
                let eval_opts = EvaluationOptions {
                    trace: mode != EvaluationTraceKind::None,
                    max_depth,
                };

                let context: zen_engine::Variable = serde_json::from_slice(&bytes).map_err(
                    |e| serde_json::json!({ "type": "ContextError", "source": e.to_string() }),
                )?;
                let response = engine
                    .evaluate_with_opts(key, context, eval_opts)
                    .await
                    .map_err(|e| {
                        e.serialize_with_mode(serde_json::value::Serializer, mode)
                            .unwrap_or_default()
                    })?;
                let serialized = response
                    .serialize_with_mode(serde_json::value::Serializer, mode)
                    .map_err(|e| {
                        serde_json::json!({ "type": "SerializeError", "source": e.to_string() })
                    })?;
                serde_json::to_vec(&serialized).map_err(
                    |e| serde_json::json!({ "type": "SerializeError", "source": e.to_string() }),
                )
            }));
        }

        let mut out = Vec::with_capacity(handles.len());
        for handle in handles {
            out.push(match handle.await {
                Ok(Ok(data)) => EvaluateBatchRawResult {
                    success: true,
                    data: Some(data.into()),
                    error: None,
                },
                Ok(Err(error)) => EvaluateBatchRawResult {
                    success: false,
                    data: None,
                    error: Some(error),
                },
                Err(_) => EvaluateBatchRawResult {
                    success: false,
                    data: None,
                    error: Some(Value::String("evaluation worker panicked".into())),
                },
            });
        }

        Ok(out)
    }

    #[napi]
    pub async fn reload(&self) -> napi::Result<()> {
        let graph = self.graph.clone();
        spawn_worker(|| async move { graph.compile() })
            .await
            .map_err(|_| anyhow!("Hook timed out"))?;
        Ok(())
    }

    #[napi(
        ts_return_type = "Array<{ key: string; kind: string; diagnostics?: Array<{ code: string; message: string; severity: string }>; error?: string }>"
    )]
    pub fn compile_failures(&self) -> napi::Result<serde_json::Value> {
        let failures = self.graph.compile_failures();
        Ok(serde_json::to_value(&failures).map_err(|e| anyhow!(e))?)
    }

    #[napi]
    pub fn dispose(&self) {
        if let Some(loader_tsfn) = &self.loader_tsfn {
            let _ = loader_tsfn.handle.dispose();
        }

        if let Some(http_handler_tsfn) = &self.http_handler_tsfn {
            let _ = http_handler_tsfn.handle.dispose();
        }

        if let Some(custom_node) = &self.custom_node_tsfn {
            let _ = custom_node.handle.dispose();
        }
    }
}

#[cfg(feature = "data")]
#[napi]
pub struct ZenImpactAnalysis {
    pub(crate) inner: Arc<zen_engine::data::impact::ImpactAnalysis>,
}

#[cfg(feature = "data")]
#[napi]
impl ZenImpactAnalysis {
    /// Candidate and baseline are full engines — they may use different
    /// loaders, documents and configuration.
    #[napi(constructor)]
    pub fn new(candidate: &ZenEngine, baseline: &ZenEngine) -> Self {
        Self {
            inner: Arc::new(zen_engine::data::impact::ImpactAnalysis::new(
                candidate.graph.clone(),
                baseline.graph.clone(),
            )),
        }
    }

    /// Merge shard aggregate states, finalize, and render the report template
    /// — the closing call of a declarative impact run.
    #[napi]
    pub fn finish(
        aggregate: Buffer,
        states: Vec<Buffer>,
        report: Option<Buffer>,
    ) -> napi::Result<String> {
        let spec: zen_engine::data::aggregate::AggregateSpec =
            serde_json::from_slice(aggregate.as_ref())
                .map_err(|e| anyhow!("aggregate is not a valid spec: {e}"))?;
        let mut aggregator = zen_engine::data::aggregate::Aggregator::compile(&spec)
            .map_err(|e| anyhow!(e.to_string()))?;
        for state in &states {
            let value: serde_json::Value = serde_json::from_slice(state.as_ref())
                .map_err(|e| anyhow!("state is not JSON: {e}"))?;
            aggregator
                .merge_value(value)
                .map_err(|e| anyhow!(e.to_string()))?;
        }
        let rendered = report
            .map(|template| -> napi::Result<serde_json::Value> {
                let template: serde_json::Value = serde_json::from_slice(template.as_ref())
                    .map_err(|e| anyhow!("report template is not JSON: {e}"))?;
                Ok(aggregator.render(&template))
            })
            .transpose()?;

        serde_json::to_string(&serde_json::json!({
            "aggregate": aggregator.finalize(),
            "report": rendered,
        }))
        .map_err(|e| anyhow!(e).into())
    }

    /// Both arms AND declarative aggregation in one native pass — per-record
    /// data never crosses the boundary; the response is the tiny additive
    /// aggregate state plus the impact summary: `{state, summary}`.
    /// Synchronous because the browser hosts run this inside a worker over the
    /// wasi build, where the async napi machinery is unreliable; evaluation
    /// futures resolve immediately off a current-thread runtime.
    #[napi]
    pub fn run_aggregate_sync(
        &self,
        candidate_key: String,
        baseline_key: String,
        inputs: Vec<Buffer>,
        aggregate: Buffer,
        start_index: Option<f64>,
        opts: Option<ZenEvaluateOptions>,
    ) -> napi::Result<String> {
        let analysis = self.inner.clone();
        let spec: zen_engine::data::aggregate::AggregateSpec =
            serde_json::from_slice(aggregate.as_ref())
                .map_err(|e| anyhow!("aggregate is not a valid spec: {e}"))?;
        let options: EvaluationSerializedOptions = opts.unwrap_or_default().into();
        let eval_options = EvaluationOptions {
            trace: false,
            max_depth: options.max_depth,
        };
        let start = start_index.unwrap_or(0.0) as u64;

        let runtime = napi::tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|e| anyhow!(e))?;

        let mut aggregator = zen_engine::data::aggregate::Aggregator::compile(&spec)
            .map_err(|e| anyhow!(e.to_string()))?;
        let comparison = runtime
            .block_on(analysis.compare(&candidate_key, &baseline_key))
            .map_err(|e| anyhow!(e.to_string()))?;

        let mut summary = zen_engine::data::impact::ImpactSummary::default();
        for (offset, payload) in inputs.iter().enumerate() {
            let input: zen_engine::Variable = serde_json::from_slice(payload.as_ref())
                .map_err(|e| anyhow!("input is not JSON: {e}"))?;
            let row = runtime.block_on(comparison.run_one_carrying(input.clone(), eval_options));
            summary.record(&row);
            aggregator.record_at(row.into_aggregate_context(input), start + offset as u64);
        }

        serde_json::to_string(&serde_json::json!({
            "state": aggregator.state(),
            "summary": summary,
        }))
        .map_err(|e| anyhow!(e).into())
    }
}

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

use crate::content::{LoadedDecision, ZenDecisionContent};
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
use zen_engine::trace::TraceFormat;
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

#[derive(Debug, Default, Clone, Copy)]
pub struct JsTraceFormat(pub TraceFormat, pub EvaluationTraceKind);

impl FromNapiValue for JsTraceFormat {
    unsafe fn from_napi_value(env: napi_env, napi_val: napi_value) -> napi::Result<Self> {
        let js_value = Unknown::from_napi_value(env, napi_val)?;

        match js_value.get_type()? {
            ValueType::Undefined | ValueType::Null => Ok(JsTraceFormat::default()),
            ValueType::Boolean => Ok(Self::switched(js_value.coerce_to_bool()?)),
            ValueType::String => {
                let utf8 = js_value.coerce_to_string()?.into_utf8()?;
                let text = utf8.as_str()?;
                if let Ok(format) = TraceFormat::from_str(text) {
                    return Ok(match format {
                        TraceFormat::Off => Self::switched(false),
                        format => JsTraceFormat(format, EvaluationTraceKind::Default),
                    });
                }
                let kind = EvaluationTraceKind::from_str(text).context("invalid trace format")?;
                Ok(match kind {
                    EvaluationTraceKind::None => Self::switched(false),
                    EvaluationTraceKind::Default => Self::switched(true),
                    kind => JsTraceFormat(TraceFormat::Legacy, kind),
                })
            }
            _ => Err(anyhow!("Invalid trace setting").into()),
        }
    }
}

impl JsTraceFormat {
    fn switched(on: bool) -> Self {
        match on {
            true => JsTraceFormat(TraceFormat::from(true), EvaluationTraceKind::Default),
            false => JsTraceFormat(TraceFormat::Off, EvaluationTraceKind::None),
        }
    }

    pub(crate) fn kind(options: &Option<ZenEvaluateOptions>) -> EvaluationTraceKind {
        options
            .as_ref()
            .and_then(|options| options.trace)
            .map(|trace| trace.1)
            .unwrap_or_default()
    }

    pub(crate) fn failure(
        error: &zen_engine::EvaluationError,
        kind: EvaluationTraceKind,
    ) -> serde_json::Value {
        match kind {
            EvaluationTraceKind::None | EvaluationTraceKind::Default => {
                serde_json::to_value(error).unwrap_or_default()
            }
            kind => error
                .serialize_with_mode(serde_json::value::Serializer, kind)
                .unwrap_or_default(),
        }
    }
}

impl ToNapiValue for JsTraceFormat {
    unsafe fn to_napi_value(env: napi_env, val: Self) -> napi::Result<napi_value> {
        match val {
            JsTraceFormat(TraceFormat::Off, _) => ToNapiValue::to_napi_value(env, false),
            JsTraceFormat(
                _,
                kind @ (EvaluationTraceKind::String
                | EvaluationTraceKind::Reference
                | EvaluationTraceKind::ReferenceString),
            ) => {
                let text: &'static str = kind.into();
                ToNapiValue::to_napi_value(env, text)
            }
            JsTraceFormat(TraceFormat::V1, _) => ToNapiValue::to_napi_value(env, true),
            JsTraceFormat(TraceFormat::Legacy, _) => ToNapiValue::to_napi_value(env, "legacy"),
        }
    }
}

#[derive(Debug)]
#[napi(object)]
pub struct ZenEvaluateOptions {
    pub max_depth: Option<u8>,
    #[napi(ts_type = "boolean | 'v1' | 'legacy' | 'string' | 'reference' | 'referenceString'")]
    pub trace: Option<JsTraceFormat>,
    #[napi(ts_type = "string | number")]
    pub now: Option<Either<String, f64>>,
    pub timezone: Option<String>,
    #[napi(ts_type = "number | string")]
    pub seed: Option<Either<f64, String>>,
}

impl Default for ZenEvaluateOptions {
    fn default() -> Self {
        Self {
            max_depth: Some(5),
            trace: Some(JsTraceFormat::default()),
            now: None,
            timezone: None,
            seed: None,
        }
    }
}

const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;

impl TryFrom<ZenEvaluateOptions> for EvaluationSerializedOptions {
    type Error = napi::Error;

    fn try_from(value: ZenEvaluateOptions) -> Result<Self, Self::Error> {
        use zen_engine::context::{instant_from_millis, parse_instant, parse_timezone};
        let now = match value.now {
            None => None,
            Some(Either::A(text)) => Some(
                parse_instant(&text)
                    .ok_or_else(|| anyhow!("invalid now '{text}', expected an RFC 3339 instant"))?,
            ),
            Some(Either::B(ms)) if ms.is_finite() && ms.fract() == 0.0 => Some(
                instant_from_millis(ms as i64)
                    .ok_or_else(|| anyhow!("invalid now {ms}, out of range"))?,
            ),
            Some(Either::B(ms)) => {
                return Err(anyhow!("invalid now {ms}, expected whole epoch milliseconds").into())
            }
        };
        let timezone = match value.timezone {
            None => None,
            Some(name) => Some(
                parse_timezone(&name)
                    .ok_or_else(|| anyhow!("invalid timezone '{name}', expected an IANA name"))?,
            ),
        };
        let seed = match value.seed {
            None => None,
            Some(Either::A(n)) if (0.0..=MAX_SAFE_INTEGER).contains(&n) && n.fract() == 0.0 => {
                Some(n as u64)
            }
            Some(Either::A(n)) => {
                return Err(anyhow!(
                    "invalid seed {n}, expected a whole number from 0 to 2^53 - 1, or a string"
                )
                .into())
            }
            Some(Either::B(text)) => Some(text.parse::<u64>().map_err(|_| {
                anyhow!("invalid seed '{text}', expected an unsigned 64-bit integer")
            })?),
        };
        Ok(Self {
            max_depth: value.max_depth.unwrap_or(5),
            trace: value.trace.unwrap_or_default().0,
            now,
            timezone,
            seed,
        })
    }
}

#[napi(object)]
pub struct EvaluateBatchRequest {
    pub key: String,
    pub context: Value,
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
    pub loader:
        Option<Either<Function<'static, String, Promise<Option<LoadedDecision>>>, Object<'static>>>,

    #[napi(ts_type = "(request: ZenEngineHandlerRequest) => Promise<ZenEngineHandlerResponse>")]
    pub custom_handler:
        Option<Function<'static, ZenEngineHandlerRequest, Promise<ZenEngineHandlerResponse>>>,

    #[napi(ts_type = "(request: ZenHttpHandlerRequest) => Promise<ZenHttpHandlerResponse>")]
    pub http_handler:
        Option<Function<'static, ZenHttpHandlerRequest, Promise<ZenHttpHandlerResponse>>>,

    #[napi(ts_type = "1 | 2")]
    pub version: Option<f64>,
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
        if let Some(version) = opts.version {
            let whole = (version.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(&version))
                .then_some(version as u32)
                .ok_or_else(|| anyhow!("unknown engine version {version}, expected 1 or 2"))?;
            decision_engine = decision_engine
                .with_version(zen_engine::EngineVersion::try_from(whole).map_err(|e| anyhow!(e))?);
        }
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
        let kind = JsTraceFormat::kind(&opts);
        let options: EvaluationOptions = opts.unwrap_or_default().try_into()?;
        let result = spawn_worker(move || async move {
            let context = zen_engine::Variable::try_from_value(context).map_err(
                |e| serde_json::json!({ "type": "ContextError", "source": e.to_string() }),
            )?;
            graph
                .evaluate_with_opts(key, context, options)
                .await
                .map(|response| NodeEvalResponse::build(response, kind))
                .map_err(|e| JsTraceFormat::failure(&e, kind))
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
        let kind = JsTraceFormat::kind(&opts);
        let options: EvaluationSerializedOptions = opts.unwrap_or_default().try_into()?;

        let mut handles = Vec::with_capacity(requests.len());
        for req in requests {
            let engine = self.graph.clone();
            let EvaluateBatchRequest { key, context } = req;
            handles.push(spawn_worker(move || async move {
                let eval_opts = options;

                let context = zen_engine::Variable::try_from_value(context).map_err(
                    |e| serde_json::json!({ "type": "ContextError", "source": e.to_string() }),
                )?;
                engine
                    .evaluate_with_opts(key, context, eval_opts)
                    .await
                    .map(|response| NodeEvalResponse::build(response, kind))
                    .map_err(|e| JsTraceFormat::failure(&e, kind))
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

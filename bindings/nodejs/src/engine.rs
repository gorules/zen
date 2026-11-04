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
use zen_engine::{DecisionEngine, EvaluationOptions, EvaluationSerializedOptions};

#[napi]
pub struct ZenEngine {
    graph: Arc<DecisionEngine>,

    custom_node_tsfn: Option<CustomNodeTsfn>,
    loader_tsfn: Option<LoaderTsfn>,
    http_handler_tsfn: Option<HttpHandlerTsfn>,
}

#[derive(Debug, Default)]
pub struct JsTraceFormat(pub TraceFormat);

impl FromNapiValue for JsTraceFormat {
    unsafe fn from_napi_value(env: napi_env, napi_val: napi_value) -> napi::Result<Self> {
        let js_value = Unknown::from_napi_value(env, napi_val)?;

        match js_value.get_type()? {
            ValueType::Undefined | ValueType::Null => Ok(JsTraceFormat::default()),
            ValueType::Boolean => Ok(JsTraceFormat(TraceFormat::from(js_value.coerce_to_bool()?))),
            ValueType::String => {
                let utf8 = js_value.coerce_to_string()?.into_utf8()?;
                let format =
                    TraceFormat::from_str(utf8.as_str()?).context("invalid trace format")?;
                Ok(JsTraceFormat(format))
            }
            _ => Err(anyhow!("Invalid trace setting").into()),
        }
    }
}

impl ToNapiValue for JsTraceFormat {
    unsafe fn to_napi_value(env: napi_env, val: Self) -> napi::Result<napi_value> {
        match val.0 {
            TraceFormat::Off => ToNapiValue::to_napi_value(env, false),
            TraceFormat::V1 => ToNapiValue::to_napi_value(env, true),
            TraceFormat::Legacy => ToNapiValue::to_napi_value(env, "legacy"),
        }
    }
}

#[derive(Debug)]
#[napi(object)]
pub struct ZenEvaluateOptions {
    pub max_depth: Option<u8>,
    #[napi(ts_type = "boolean | 'v1' | 'legacy'")]
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

impl TryFrom<ZenEvaluateOptions> for EvaluationSerializedOptions {
    type Error = napi::Error;

    fn try_from(value: ZenEvaluateOptions) -> Result<Self, Self::Error> {
        use zen_engine::clock::{instant_from_millis, parse_instant, parse_timezone};
        let now = match value.now {
            None => None,
            Some(Either::A(text)) => Some(
                parse_instant(&text).ok_or_else(|| anyhow!("invalid now, expected RFC 3339"))?,
            ),
            Some(Either::B(ms)) if ms.fract() == 0.0 => {
                Some(instant_from_millis(ms as i64).ok_or_else(|| anyhow!("invalid now"))?)
            }
            Some(Either::B(_)) => return Err(anyhow!("now must be whole milliseconds").into()),
        };
        let timezone = match value.timezone {
            None => None,
            Some(name) => {
                Some(parse_timezone(&name).ok_or_else(|| anyhow!("invalid timezone {name}"))?)
            }
        };
        let seed = match value.seed {
            None => None,
            Some(Either::A(n)) if n >= 0.0 && n.fract() == 0.0 && n < 9007199254740992.0 => {
                Some(n as u64)
            }
            Some(Either::A(_)) => {
                return Err(anyhow!("seed must be a whole number below 2^53, or a string").into())
            }
            Some(Either::B(text)) => Some(
                text.parse::<u64>()
                    .map_err(|_| anyhow!("invalid seed {text}"))?,
            ),
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
        let options: EvaluationOptions = opts.unwrap_or_default().try_into()?;
        let result = spawn_worker(move || async move {
            let context = zen_engine::Variable::try_from_value(context).map_err(
                |e| serde_json::json!({ "type": "ContextError", "source": e.to_string() }),
            )?;
            graph
                .evaluate_with_opts(key, context, options)
                .await
                .map(NodeEvalResponse::build)
                .map_err(|e| serde_json::to_value(&*e).unwrap_or_default())
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
                    .map(NodeEvalResponse::build)
                    .map_err(|e| serde_json::to_value(&*e).unwrap_or_default())
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

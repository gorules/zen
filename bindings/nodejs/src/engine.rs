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
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationSerializedOptions, EvaluationTraceKind};

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
pub struct ZenEngineOptions {
    #[napi(ts_type = "(key: string) => Promise<Buffer | ZenDecisionContent>")]
    pub loader: Option<
        Function<'static, String, Promise<Option<Either<Buffer, &'static ZenDecisionContent>>>>,
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
    pub fn new(options: Option<ZenEngineOptions>) -> napi::Result<Self> {
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

        let loader = match opts.loader {
            None => DecisionLoader::default(),
            Some(l) => {
                let loader_tsfn = l
                    .build_threadsafe_function()
                    .max_queue_size::<0>()
                    .callee_handled::<false>()
                    .weak()
                    .build()?;

                let arc_loader_tsfn = Arc::new(loader_tsfn);
                loader_tsfn_opt = Some(arc_loader_tsfn.clone());
                DecisionLoader::new(arc_loader_tsfn)
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

        let mut decision_engine = DecisionEngine::new(Arc::new(loader), Arc::new(custom_node));
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
    ) -> napi::Result<Value> {
        let graph = self.graph.clone();
        let result = spawn_worker(|| {
            let options = opts.unwrap_or_default();

            async move {
                graph
                    .evaluate_serialized(key, context.into(), options.into())
                    .await
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

        let decision = self.graph.create_decision(decision_content);
        Ok(ZenDecision::from(decision))
    }

    #[napi]
    pub async fn get_decision(&self, key: String) -> napi::Result<ZenDecision> {
        let decision = self
            .graph
            .get_decision(&key)
            .await
            .with_context(|| format!("Failed to find decision with key = {key}"))?;

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
    ) -> SafeResult<Value> {
        self.evaluate(key, context, opts).await.into()
    }

    #[napi(
        ts_return_type = "Promise<{ success: true, data: ZenDecision } | { success: false; error: any; }>"
    )]
    pub async fn safe_get_decision(&self, key: String) -> SafeResult<ZenDecision> {
        self.get_decision(key).await.into()
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

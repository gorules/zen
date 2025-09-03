use std::str::FromStr;
use std::sync::Arc;

use napi::anyhow::{anyhow, Context};
use napi::bindgen_prelude::{Buffer, Either3, FromNapiValue, ToNapiValue};
use napi::sys::{napi_env, napi_value};
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction};
use napi::{Env, JsFunction, JsObject, JsUnknown, NapiValue, ValueType};
use napi_derive::napi;
use serde_json::Value;

use crate::content::ZenDecisionContent;
use crate::custom_node::CustomNode;
use crate::decision::ZenDecision;
use crate::loader::DecisionLoader;
use crate::mt::spawn_worker;
use crate::safe_result::SafeResult;
use crate::types::ZenEngineHandlerRequest;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationSerializedOptions, EvaluationTraceKind};

#[napi]
pub struct ZenEngine {
    graph: Arc<DecisionEngine>,

    loader_ref: Option<ThreadsafeFunction<String, ErrorStrategy::Fatal>>,
    custom_handler_ref: Option<ThreadsafeFunction<ZenEngineHandlerRequest, ErrorStrategy::Fatal>>,
}

#[derive(Debug, Default)]
pub struct JsEvaluationTraceKind(pub EvaluationTraceKind);

impl FromNapiValue for JsEvaluationTraceKind {
    unsafe fn from_napi_value(env: napi_env, napi_val: napi_value) -> napi::Result<Self> {
        let js_value = JsUnknown::from_raw(env, napi_val)?;

        match js_value.get_type()? {
            ValueType::Undefined | ValueType::Null => Ok(JsEvaluationTraceKind::default()),
            ValueType::Boolean => {
                let enabled = js_value.coerce_to_bool()?.get_value()?;
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
    pub loader: Option<JsFunction>,

    #[napi(ts_type = "(request: ZenEngineHandlerRequest) => Promise<ZenEngineHandlerResponse>")]
    pub custom_handler: Option<JsFunction>,
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

                loader_ref: None,
                custom_handler_ref: None,
            });
        };

        let loader_ref = match opts.loader {
            None => None,
            Some(l) => Some(l.create_threadsafe_function(
                0,
                |cx: ThreadSafeCallContext<String>| {
                    cx.env.create_string(cx.value.as_str()).map(|v| vec![v])
                },
            )?),
        };

        let loader = match &loader_ref {
            None => DecisionLoader::default(),
            Some(loader_fn) => DecisionLoader::new(loader_fn.clone())?,
        };

        let custom_handler_ref = match opts.custom_handler {
            None => None,
            Some(custom_handler_fn) => Some(custom_handler_fn.create_threadsafe_function(
                0,
                |cx: ThreadSafeCallContext<ZenEngineHandlerRequest>| Ok(vec![cx.value]),
            )?),
        };

        let custom_handler = match &custom_handler_ref {
            None => CustomNode::default(),
            Some(custom_fn) => CustomNode::new(custom_fn.clone()),
        };

        Ok(Self {
            graph: DecisionEngine::new(Arc::new(loader), Arc::new(custom_handler)).into(),

            loader_ref,
            custom_handler_ref,
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
        content: Either3<&ZenDecisionContent, Buffer, JsObject>,
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

        Ok(ZenDecision::from(decision))
    }

    #[napi(ts_return_type = "Promise<SafeResult<ZenEngineResponse>>")]
    pub async fn safe_evaluate(
        &self,
        key: String,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
    ) -> SafeResult<Value> {
        self.evaluate(key, context, opts).await.into()
    }

    #[napi(ts_return_type = "Promise<SafeResult<ZenDecision>>")]
    pub async fn safe_get_decision(&self, key: String) -> SafeResult<ZenDecision> {
        self.get_decision(key).await.into()
    }

    /// Function used to dispose memory allocated for loaders
    /// In the future, it will likely be removed and made automatic
    #[napi]
    pub fn dispose(&self) {
        if let Some(loader) = self.loader_ref.clone() {
            let _ = loader.abort();
        }

        if let Some(loader) = self.custom_handler_ref.clone() {
            let _ = loader.abort();
        }
    }
}

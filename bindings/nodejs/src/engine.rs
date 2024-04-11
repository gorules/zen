use crate::content::ZenDecisionContent;
use crate::custom_node::CustomNode;
use crate::decision::ZenDecision;
use crate::loader::DecisionLoader;
use crate::types::{ZenEngineHandlerRequest, ZenEngineResponse};
use napi::anyhow::{anyhow, Context};
use napi::bindgen_prelude::{Buffer, Either3};
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction};
use napi::{tokio, Env, JsFunction, JsObject};
use napi_derive::napi;
use serde_json::Value;
use std::sync::Arc;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationOptions};

#[napi]
pub struct ZenEngine {
    graph: Arc<DecisionEngine<DecisionLoader, CustomNode>>,

    loader_ref: Option<ThreadsafeFunction<String, ErrorStrategy::Fatal>>,
    custom_handler_ref: Option<ThreadsafeFunction<ZenEngineHandlerRequest, ErrorStrategy::Fatal>>,
}

#[napi(object)]
pub struct ZenEvaluateOptions {
    pub max_depth: Option<u8>,
    pub trace: Option<bool>,
}

impl Default for ZenEvaluateOptions {
    fn default() -> Self {
        Self {
            max_depth: Some(5),
            trace: Some(false),
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
                    DecisionLoader::default().into(),
                    CustomNode::default().into(),
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
            graph: DecisionEngine::new(loader.into(), custom_handler.into()).into(),

            loader_ref,
            custom_handler_ref,
        })
    }

    #[napi]
    pub async fn evaluate(
        &self,
        key: String,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
    ) -> napi::Result<ZenEngineResponse> {
        let graph = self.graph.clone();
        let result = tokio::spawn(async move {
            let options = opts.unwrap_or_default();

            futures::executor::block_on(graph.evaluate_with_opts(
                key,
                &context,
                EvaluationOptions {
                    max_depth: options.max_depth,
                    trace: options.trace,
                },
            ))
        })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
        .map_err(|e| {
            anyhow!(serde_json::to_string(e.as_ref()).unwrap_or_else(|_| e.to_string()))
        })?;

        Ok(ZenEngineResponse::from(result))
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

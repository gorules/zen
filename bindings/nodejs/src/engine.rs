use crate::decision::JsZenDecision;
use crate::loader::JsDecisionLoader;
use napi::anyhow::{anyhow, Context};
use napi::bindgen_prelude::Buffer;
use napi::{tokio, JsFunction};
use napi_derive::napi;
use serde_json::Value;
use std::sync::Arc;
use zen_engine::engine::{DecisionEngine, EvaluationOptions};
use zen_engine::model::decision::DecisionContent;

#[napi(js_name = "ZenEngine")]
pub struct JsZenEngine {
    graph: Arc<DecisionEngine<JsDecisionLoader>>,
}

#[napi(object, js_name = "ZenEvaluateOptions")]
pub struct JsZenEvaluateOptions {
    pub max_depth: Option<u8>,
    pub trace: Option<bool>,
}

impl Default for JsZenEvaluateOptions {
    fn default() -> Self {
        Self {
            max_depth: Some(5),
            trace: Some(false),
        }
    }
}

#[napi(object)]
pub struct JsZenEngineOptions {
    #[napi(ts_type = "(key: string) => Promise<Buffer>")]
    pub loader: Option<JsFunction>,
}

#[napi]
impl JsZenEngine {
    #[napi(constructor)]
    pub fn new(options: Option<JsZenEngineOptions>) -> napi::Result<Self> {
        let Some(opts) = options else {
          return Ok(Self { graph: DecisionEngine::new(JsDecisionLoader::default()).into() })
        };

        let Some(loader_fn) = opts.loader else {
            return Ok(Self { graph: DecisionEngine::new(JsDecisionLoader::default()).into() })
        };

        Ok(Self {
            graph: DecisionEngine::new(JsDecisionLoader::try_from(loader_fn)?).into(),
        })
    }

    #[napi]
    pub async fn evaluate(
        &self,
        key: String,
        context: Value,
        opts: Option<JsZenEvaluateOptions>,
    ) -> napi::Result<Value> {
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
        .map_err(|_| anyhow!("Hook timed out"))??;

        Ok(serde_json::to_value(&result)?)
    }

    #[napi]
    pub fn create_decision(&self, content: Buffer) -> napi::Result<JsZenDecision> {
        let decision_content: DecisionContent = serde_json::from_slice(content.as_ref())?;
        let decision = self.graph.create_decision(Arc::new(decision_content));
        Ok(JsZenDecision::from(decision))
    }

    #[napi]
    pub async fn get_decision(&self, key: String) -> napi::Result<JsZenDecision> {
        let decision = self
            .graph
            .get_decision(&key)
            .await
            .context("Failed to find decision with given key")?;

        Ok(JsZenDecision::from(decision))
    }
}

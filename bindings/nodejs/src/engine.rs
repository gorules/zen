use crate::decision::ZenDecision;
use crate::loader::DecisionLoader;
use napi::anyhow::{anyhow, Context};
use napi::bindgen_prelude::Buffer;
use napi::{tokio, JsFunction};
use napi_derive::napi;
use serde_json::Value;
use std::sync::Arc;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationOptions};

#[napi]
pub struct ZenEngine {
    graph: Arc<DecisionEngine<DecisionLoader>>,
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
    #[napi(ts_type = "(key: string) => Promise<Buffer>")]
    pub loader: Option<JsFunction>,
}

#[napi]
impl ZenEngine {
    #[napi(constructor)]
    pub fn new(options: Option<ZenEngineOptions>) -> napi::Result<Self> {
        let Some(opts) = options else {
          return Ok(Self { graph: DecisionEngine::new(DecisionLoader::default()).into() })
        };

        let Some(loader_fn) = opts.loader else {
            return Ok(Self { graph: DecisionEngine::new(DecisionLoader::default()).into() })
        };

        Ok(Self {
            graph: DecisionEngine::new(DecisionLoader::try_from(loader_fn)?).into(),
        })
    }

    #[napi]
    pub async fn evaluate(
        &self,
        key: String,
        context: Value,
        opts: Option<ZenEvaluateOptions>,
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
        .map_err(|_| anyhow!("Hook timed out"))?
        .map_err(|e| anyhow!(e))?;

        Ok(serde_json::to_value(&result)?)
    }

    #[napi]
    pub fn create_decision(&self, content: Buffer) -> napi::Result<ZenDecision> {
        let decision_content: DecisionContent = serde_json::from_slice(content.as_ref())?;
        let decision = self.graph.create_decision(Arc::new(decision_content));
        Ok(ZenDecision::from(decision))
    }

    #[napi]
    pub async fn get_decision(&self, key: String) -> napi::Result<ZenDecision> {
        let decision = self
            .graph
            .get_decision(&key)
            .await
            .context("Failed to find decision with given key")?;

        Ok(ZenDecision::from(decision))
    }
}

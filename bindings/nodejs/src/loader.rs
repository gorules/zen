use async_trait::async_trait;
use napi::anyhow::anyhow;
use napi::bindgen_prelude::{Buffer, Promise};
use napi::threadsafe_function::{ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction};
use napi::{Env, JsFunction};

use std::sync::Arc;

use zen_engine::loader::{DecisionLoader as DecisionLoaderTrait, LoaderError, LoaderResult};
use zen_engine::model::DecisionContent;

pub(crate) struct DecisionLoader {
    function: Option<ThreadsafeFunction<String, ErrorStrategy::Fatal>>,
}

impl Default for DecisionLoader {
    fn default() -> Self {
        Self { function: None }
    }
}

impl DecisionLoader {
    pub fn try_new(env: &mut Env, function: JsFunction) -> napi::Result<Self> {
        let mut tsf =
            function.create_threadsafe_function(0, |cx: ThreadSafeCallContext<String>| {
                cx.env.create_string(cx.value.as_str()).map(|v| vec![v])
            })?;

        tsf.unref(env)?;

        Ok(Self {
            function: Some(tsf),
        })
    }

    pub async fn get_key(&self, key: &str) -> LoaderResult<Arc<DecisionContent>> {
        let Some(function) = &self.function else {
            return Err(LoaderError::Internal {
                key: key.to_string(),
                source: anyhow!("Loader is undefined"),
            }
            .into());
        };

        let promise: Promise<Option<Buffer>> = function
            .clone()
            .call_async(key.to_string())
            .await
            .map_err(|e| LoaderError::Internal {
            key: key.to_string(),
            source: e.into(),
        })?;

        let result = promise.await.map_err(|e| LoaderError::Internal {
            key: key.to_string(),
            source: e.into(),
        })?;

        let Some(buffer) = result else {
            return Err(LoaderError::NotFound(key.to_string()).into());
        };

        let decision_content =
            serde_json::from_slice(buffer.as_ref()).map_err(|e| LoaderError::Internal {
                key: key.to_string(),
                source: e.into(),
            })?;

        Ok(Arc::new(decision_content))
    }
}

#[async_trait]
impl DecisionLoaderTrait for DecisionLoader {
    async fn load(&self, key: &str) -> LoaderResult<Arc<DecisionContent>> {
        let decision_content = self.get_key(key).await?;
        Ok(decision_content)
    }
}

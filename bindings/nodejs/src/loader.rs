use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use napi::anyhow::anyhow;
use napi::bindgen_prelude::{Buffer, Promise};
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction};
use napi::Either;

use zen_engine::loader::{
    DecisionLoader as DecisionLoaderTrait, LoaderError, LoaderResponse, LoaderResult,
};
use zen_engine::model::DecisionContent;

use crate::content::ZenDecisionContent;

#[derive(Default)]
pub(crate) struct DecisionLoader {
    function: Option<ThreadsafeFunction<String, ErrorStrategy::Fatal>>,
}

impl Debug for DecisionLoader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "DecisionLoader")
    }
}

impl DecisionLoader {
    pub fn new(tsf: ThreadsafeFunction<String, ErrorStrategy::Fatal>) -> napi::Result<Self> {
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

        let promise: Promise<Option<Either<Buffer, &ZenDecisionContent>>> = function
            .clone()
            .call_async(key.to_string())
            .await
            .map_err(|e| LoaderError::Internal {
                key: key.to_string(),
                source: anyhow!(e.reason),
            })?;

        let result = promise.await.map_err(|e| LoaderError::Internal {
            key: key.to_string(),
            source: anyhow!(e.reason),
        })?;

        let Some(buffer) = result else {
            return Err(LoaderError::NotFound(key.to_string()).into());
        };

        let decision_content = match buffer {
            Either::A(buf) => Arc::new(serde_json::from_slice(buf.as_ref()).map_err(|e| {
                LoaderError::Internal {
                    key: key.to_string(),
                    source: e.into(),
                }
            })?),
            Either::B(dc) => dc.inner.clone(),
        };

        Ok(decision_content)
    }
}

impl DecisionLoaderTrait for DecisionLoader {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + Send + 'a>> {
        Box::pin(async move {
            let decision_content = self.get_key(key).await?;
            Ok(decision_content)
        })
    }
}

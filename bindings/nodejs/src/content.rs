use std::sync::Arc;

use napi::bindgen_prelude::{Buffer, Object};
use napi::{Either, Env};
use napi_derive::napi;
use serde_json::Value;

use zen_engine::model::DecisionContent;

#[napi]
pub struct ZenDecisionContent {
    pub(crate) inner: Arc<DecisionContent>,
}

#[napi]
impl ZenDecisionContent {
    #[napi(constructor)]
    pub fn new(env: Env, content: Either<Buffer, Object>) -> napi::Result<Self> {
        let mut decision_content: DecisionContent = match content {
            Either::A(buf) => serde_json::from_slice(buf.as_ref())?,
            Either::B(obj) => {
                let serde_val: Value = env.from_js_value(obj)?;
                serde_json::from_value(serde_val)?
            }
        };
        decision_content.compile();

        Ok(Self {
            inner: Arc::new(decision_content),
        })
    }

    #[napi]
    pub fn to_buffer(&self) -> napi::Result<Buffer> {
        let content_vec = serde_json::to_vec(&self.inner.as_ref())?;
        Ok(Buffer::from(content_vec))
    }
}

use std::sync::Arc;

use napi::bindgen_prelude::{Buffer, FromNapiValue, Object, ValidateNapiValue};
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
        let decision_content: DecisionContent = match content {
            Either::A(buf) => serde_json::from_slice(buf.as_ref())?,
            Either::B(obj) => {
                let serde_val: Value = env.from_js_value(obj)?;
                serde_json::from_value(serde_val)?
            }
        };

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

// impl FromNapiValue for ZenDecisionContent {
//     unsafe fn from_napi_value(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<Self> {
//         // Convert the JS value to an Object first
//         let obj = Object::from_napi_value(env, napi_val)?;
//
//         // Use your existing constructor logic
//         let env_wrapper = Env::from_raw(env);
//         Self::new(env_wrapper, Either::B(obj))
//     }
// }
//
// impl ValidateNapiValue for ZenDecisionContent {
//     unsafe fn validate(env: napi::sys::napi_env, napi_val: napi::sys::napi_value) -> napi::Result<napi::sys::napi_value> {
//         // Validate that the value can be converted to an Object
//         // This is the most common validation for complex types
//         Object::validate(env, napi_val)
//     }
// }
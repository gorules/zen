use std::sync::Arc;

use napi::bindgen_prelude::{
    sys, Buffer, ClassInstance, FromNapiValue, Object, TypeName, ValidateNapiValue,
};
use napi::{Either, Env, ValueType};
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
        if let DecisionContent::Graph(g) = &mut decision_content {
            Arc::make_mut(g).compile();
        }

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

pub enum LoadedDecision {
    Content(Arc<DecisionContent>),
    Bytes(Buffer),
}

impl TypeName for LoadedDecision {
    fn type_name() -> &'static str {
        "Buffer | ZenDecisionContent"
    }

    fn value_type() -> ValueType {
        ValueType::Unknown
    }
}

impl ValidateNapiValue for LoadedDecision {}

impl FromNapiValue for LoadedDecision {
    unsafe fn from_napi_value(env: sys::napi_env, napi_val: sys::napi_value) -> napi::Result<Self> {
        if let Ok(instance) =
            unsafe { ClassInstance::<ZenDecisionContent>::from_napi_value(env, napi_val) }
        {
            return Ok(Self::Content(instance.inner.clone()));
        }

        Ok(Self::Bytes(unsafe {
            Buffer::from_napi_value(env, napi_val)
        }?))
    }
}

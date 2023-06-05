use std::ffi::CStr;
use std::sync::Arc;

use async_trait::async_trait;
use libc::c_char;

use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;

use crate::LoaderFn;

#[derive(Default)]
pub struct FfiLoader {
    handler: Option<LoaderFn>,
}

impl FfiLoader {
    pub fn new(function: LoaderFn) -> Self {
        Self {
            handler: Some(function),
        }
    }
}

#[async_trait]
impl DecisionLoader for FfiLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        let Some(handler) = &self.handler else {
            return Err(LoaderError::NotFound(key.to_string()).into())
        };

        let c_key = unsafe { CStr::from_bytes_with_nul_unchecked(key.as_bytes()) };
        let content_c_ptr = handler(c_key.as_ptr());
        let c_content = unsafe { CStr::from_ptr(content_c_ptr) };
        let k = c_content.to_str().unwrap();
        let decision_content: DecisionContent = serde_json::from_str(k).unwrap();

        Ok(Arc::new(decision_content))
    }
}

use anyhow::anyhow;
use async_trait::async_trait;
use std::ffi::{c_char, CString};
use std::sync::Arc;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;

pub type CZenDecisionLoaderCallback = extern "C" fn(key: *const c_char) -> CZenDecisionLoaderResult;

#[repr(C)]
pub struct CZenDecisionLoaderResult {
    content: *mut c_char,
    error: *mut c_char,
}

impl CZenDecisionLoaderResult {
    pub fn into_loader_response(self, key: &str) -> LoaderResponse {
        let maybe_error = match self.error.is_null() {
            false => Some(unsafe { CString::from_raw(self.error) }),
            true => None,
        };

        let maybe_content = match self.content.is_null() {
            false => Some(unsafe { CString::from_raw(self.content) }),
            true => None,
        };

        if let Some(c_error) = maybe_error {
            return Err(LoaderError::Internal {
                key: key.to_string(),
                source: anyhow!(c_error.to_string_lossy().to_string()),
            }
            .into());
        }

        // If both pointers are null, we are treating it as not found
        let Some(c_content) = maybe_content else {
            return Err(LoaderError::NotFound(key.to_string()).into())
        };

        let content = c_content.into_string().map_err(|e| LoaderError::Internal {
            key: key.to_string(),
            source: anyhow!(e),
        })?;

        let decision_content: DecisionContent =
            serde_json::from_str(&content).map_err(|e| LoaderError::Internal {
                key: key.to_string(),
                source: anyhow!(e),
            })?;

        Ok(Arc::new(decision_content))
    }
}

pub(crate) struct CDecisionLoader {
    callback: CZenDecisionLoaderCallback,
}

impl CDecisionLoader {
    pub fn new(callback: CZenDecisionLoaderCallback) -> Self {
        Self { callback }
    }
}

#[async_trait]
impl DecisionLoader for CDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        let c_key = CString::new(key).unwrap();
        let c_content_ptr = (&self.callback)(c_key.as_ptr());

        c_content_ptr.into_loader_response(key)
    }
}

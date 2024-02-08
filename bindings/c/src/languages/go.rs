use std::ffi::c_char;

use async_trait::async_trait;

use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};

use crate::engine::ZenEngine;
use crate::loader::{DynamicDecisionLoader, ZenDecisionLoaderResult};

#[derive(Debug, Default)]
pub(crate) struct GoDecisionLoader {
    #[allow(dead_code)]
    handler: Option<usize>,
}

impl GoDecisionLoader {
    pub fn new(handler: Option<usize>) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl DecisionLoader for GoDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        let Some(handler) = &self.handler else {
            return Err(LoaderError::NotFound(key.to_string()).into());
        };

        let c_key = std::ffi::CString::new(key).unwrap();
        let c_content_ptr =
            unsafe { zen_engine_go_loader_callback(handler.clone(), c_key.as_ptr()) };

        c_content_ptr.into_loader_response(key)
    }
}

/// Creates a DecisionEngine for using GoLang handler (optional). Caller is responsible for freeing DecisionEngine.  
#[no_mangle]
pub extern "C" fn zen_engine_new_with_go_loader(maybe_loader: Option<&usize>) -> *mut ZenEngine {
    let loader = GoDecisionLoader::new(maybe_loader.cloned());
    let engine = ZenEngine::with_loader(DynamicDecisionLoader::Go(loader));

    Box::into_raw(Box::new(engine))
}

#[allow(unused_doc_comments)]
/// cbindgen:ignore
extern "C" {
    fn zen_engine_go_loader_callback(cb_ptr: usize, key: *const c_char) -> ZenDecisionLoaderResult;
}

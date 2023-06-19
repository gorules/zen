use crate::helpers::types::{CZenDecisionEngine, CZenDecisionEnginePtr, DynDecisionLoader};
use crate::loader::CZenDecisionLoaderResult;
use async_trait::async_trait;
use std::ffi::{c_char, CString};
use std::sync::Arc;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::DecisionEngine;

/// Creates a DecisionEngine for using GoLang handler (optional). Caller is responsible for freeing DecisionEngine.  
#[no_mangle]
pub extern "C" fn zen_engine_new_with_go_loader(
    maybe_loader: Option<&usize>,
) -> *mut CZenDecisionEnginePtr {
    let loader = Arc::new(CGoDecisionLoader::new(maybe_loader.cloned()));
    let engine: CZenDecisionEngine = DecisionEngine::new(DynDecisionLoader::new(loader));

    Box::into_raw(Box::new(engine)) as *mut CZenDecisionEnginePtr
}

#[allow(unused_doc_comments)]
/// cbindgen:ignore
extern "C" {
    fn zen_engine_go_loader_callback(cb_ptr: usize, key: *const c_char)
        -> CZenDecisionLoaderResult;
}

#[derive(Default)]
pub(crate) struct CGoDecisionLoader {
    handler: Option<usize>,
}

impl CGoDecisionLoader {
    pub fn new(handler: Option<usize>) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl DecisionLoader for CGoDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        let Some(handler) = &self.handler else {
            return Err(LoaderError::NotFound(key.to_string()).into())
        };

        let c_key = CString::new(key).unwrap();
        let c_content_ptr =
            unsafe { zen_engine_go_loader_callback(handler.clone(), c_key.as_ptr()) };

        c_content_ptr.into_loader_response(key)
    }
}

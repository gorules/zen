use std::ffi::{c_char, CString};

use async_trait::async_trait;

use zen_engine::loader::{DecisionLoader, LoaderResponse};

use crate::engine::ZenEngine;
use crate::loader::{DynamicDecisionLoader, ZenDecisionLoaderResult};

pub type ZenDecisionLoaderNativeCallback =
    extern "C" fn(key: *const c_char) -> ZenDecisionLoaderResult;

#[derive(Debug)]
pub(crate) struct NativeDecisionLoader {
    callback: ZenDecisionLoaderNativeCallback,
}

impl NativeDecisionLoader {
    pub fn new(callback: ZenDecisionLoaderNativeCallback) -> Self {
        Self { callback }
    }
}

#[async_trait]
impl DecisionLoader for NativeDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        let c_key = CString::new(key).unwrap();
        let c_content_ptr = (&self.callback)(c_key.as_ptr());

        c_content_ptr.into_loader_response(key)
    }
}

/// Creates a new ZenEngine instance with loader, caller is responsible for freeing the returned reference
/// by calling zen_engine_free.
#[no_mangle]
pub extern "C" fn zen_engine_new_with_native_loader(
    callback: ZenDecisionLoaderNativeCallback,
) -> *mut ZenEngine {
    let loader = NativeDecisionLoader::new(callback);
    let engine = ZenEngine::with_loader(DynamicDecisionLoader::Native(loader));

    Box::into_raw(Box::new(engine))
}

use anyhow::anyhow;
use std::ffi::{c_char, CString};

use async_trait::async_trait;
use zen_engine::handler::custom_node_adapter::CustomNodeAdapter;
use zen_engine::handler::node::{NodeRequest, NodeResult};

use crate::custom_node::{DynamicCustomNode, ZenCustomNodeResult};
use zen_engine::loader::{DecisionLoader, LoaderResponse};

use crate::engine::{ZenEngine, ZenEngineStruct};
use crate::loader::{DynamicDecisionLoader, ZenDecisionLoaderResult};

pub type ZenDecisionLoaderNativeCallback =
    extern "C" fn(key: *const c_char) -> ZenDecisionLoaderResult;

pub type ZenCustomNodeNativeCallback = extern "C" fn(request: *const c_char) -> ZenCustomNodeResult;

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

#[derive(Debug)]
pub(crate) struct NativeCustomNode {
    callback: ZenCustomNodeNativeCallback,
}

impl NativeCustomNode {
    pub fn new(callback: ZenCustomNodeNativeCallback) -> Self {
        Self { callback }
    }
}

impl CustomNodeAdapter for NativeCustomNode {
    async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let Ok(request_value) = serde_json::to_string(request) else {
            return Err(anyhow!("failed to serialize request json"));
        };

        let c_request = unsafe { CString::from_vec_unchecked(request_value.into_bytes()) };
        let c_response_str = (&self.callback)(c_request.as_ptr());
        c_response_str.into_node_result()
    }
}

/// Creates a new ZenEngine instance with loader, caller is responsible for freeing the returned reference
/// by calling zen_engine_free.
#[no_mangle]
pub extern "C" fn zen_engine_new_native(
    loader_callback: ZenDecisionLoaderNativeCallback,
    custom_node_callback: ZenCustomNodeNativeCallback,
) -> *mut ZenEngineStruct {
    let loader = NativeDecisionLoader::new(loader_callback);
    let custom_node = NativeCustomNode::new(custom_node_callback);

    let engine = ZenEngine::new(
        DynamicDecisionLoader::Native(loader),
        DynamicCustomNode::Native(custom_node),
    );

    Box::into_raw(Box::new(engine)) as *mut ZenEngineStruct
}

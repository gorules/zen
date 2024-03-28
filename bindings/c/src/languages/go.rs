use std::ffi::{c_char, CString};

use anyhow::anyhow;
use async_trait::async_trait;

use zen_engine::handler::custom_node_adapter::CustomNodeAdapter;
use zen_engine::handler::node::{NodeRequest, NodeResult};
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};

use crate::custom_node::{DynamicCustomNode, ZenCustomNodeResult};
use crate::engine::{ZenEngine, ZenEngineStruct};
use crate::loader::{DynamicDecisionLoader, ZenDecisionLoaderResult};

#[derive(Debug, Default)]
pub(crate) struct GoDecisionLoader {
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

        let c_key = CString::new(key).unwrap();
        let c_content_ptr =
            unsafe { zen_engine_go_loader_callback(handler.clone(), c_key.as_ptr()) };

        c_content_ptr.into_loader_response(key)
    }
}

#[derive(Debug, Default)]
pub(crate) struct GoCustomNode {
    #[allow(dead_code)]
    handler: Option<usize>,
}

impl GoCustomNode {
    pub fn new(handler: Option<usize>) -> Self {
        Self { handler }
    }
}

impl CustomNodeAdapter for GoCustomNode {
    async fn handle(&self, request: &NodeRequest<'_>) -> NodeResult {
        let Some(handler) = self.handler else {
            return Err(anyhow!("go handler not found"));
        };

        let Ok(request_value) = serde_json::to_string(request) else {
            return Err(anyhow!("failed to serialize request json"));
        };

        let c_request = unsafe { CString::from_vec_unchecked(request_value.into_bytes()) };
        let c_node_result =
            unsafe { zen_engine_go_custom_node_callback(handler, c_request.as_ptr()) };
        c_node_result.into_node_result()
    }
}

fn map_handler(i: Option<usize>) -> Option<usize> {
    let Some(j) = i else { return None };

    (j > 0).then_some(j)
}

/// Creates a DecisionEngine for using GoLang handler (optional). Caller is responsible for freeing DecisionEngine.  
#[no_mangle]
pub extern "C" fn zen_engine_new_golang(
    maybe_loader: Option<&usize>,
    maybe_custom_node: Option<&usize>,
) -> *mut ZenEngineStruct {
    let loader = GoDecisionLoader::new(map_handler(maybe_loader.cloned()));
    let custom_node = GoCustomNode::new(map_handler(maybe_custom_node.cloned()));
    let engine = ZenEngine::new(
        DynamicDecisionLoader::Go(loader),
        DynamicCustomNode::Go(custom_node),
    );

    Box::into_raw(Box::new(engine)) as *mut ZenEngineStruct
}

#[allow(unused_doc_comments)]
/// cbindgen:ignore
extern "C" {
    fn zen_engine_go_loader_callback(cb_ptr: usize, key: *const c_char) -> ZenDecisionLoaderResult;
    fn zen_engine_go_custom_node_callback(
        cb_ptr: usize,
        request: *const c_char,
    ) -> ZenCustomNodeResult;
}

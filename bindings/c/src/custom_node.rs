use crate::languages::native::NativeCustomNode;
use anyhow::anyhow;
use std::ffi::{c_char, CString};
use std::future::Future;
use std::pin::Pin;
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest, NoopCustomNode};
use zen_engine::nodes::{NodeResponse, NodeResult};

#[derive(Debug)]
pub(crate) enum DynamicCustomNode {
    Noop(NoopCustomNode),
    Native(NativeCustomNode),
    #[cfg(feature = "go")]
    Go(crate::languages::go::GoCustomNode),
}

impl Default for DynamicCustomNode {
    fn default() -> Self {
        Self::Noop(Default::default())
    }
}

impl CustomNodeAdapter for DynamicCustomNode {
    fn handle(&self, request: CustomNodeRequest) -> Pin<Box<dyn Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            match self {
                DynamicCustomNode::Noop(cn) => cn.handle(request).await,
                DynamicCustomNode::Native(cn) => cn.handle(request).await,
                #[cfg(feature = "go")]
                DynamicCustomNode::Go(cn) => cn.handle(request).await,
            }
        })
    }
}

#[repr(C)]
pub struct ZenCustomNodeResult {
    content: *mut c_char,
    error: *mut c_char,
}

impl ZenCustomNodeResult {
    pub fn into_node_result(self) -> anyhow::Result<NodeResponse> {
        let maybe_error = match self.error.is_null() {
            false => Some(unsafe { CString::from_raw(self.error) }),
            true => None,
        };

        if let Some(c_error) = maybe_error {
            let maybe_str = c_error.to_str().unwrap_or("unknown error");
            return Err(anyhow!("{maybe_str}"));
        }

        if self.content.is_null() {
            return Err(anyhow!("response not provided"));
        }

        let content_cstr = unsafe { CString::from_raw(self.content) };
        let node_response: NodeResponse = serde_json::from_slice(content_cstr.to_bytes())
            .map_err(|_| anyhow!("failed to deserialize"))?;

        Ok(node_response)
    }
}

use crate::error::ZenError;
use crate::types::{DecisionNode, JsonBuffer, ZenEngineHandlerRequest, ZenEngineHandlerResponse};
use serde_json::Value;
use std::sync::Arc;
use uniffi::deps::anyhow::anyhow;
use zen_engine::handler::custom_node_adapter::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::handler::node::{NodeResponse, NodeResult};
use zen_expression::Variable;

#[uniffi::export(callback_interface)]
#[async_trait::async_trait]
pub trait ZenCustomNodeCallback: Send + Sync {
    async fn handle(
        &self,
        key: ZenEngineHandlerRequest,
    ) -> Result<ZenEngineHandlerResponse, ZenError>;
}

pub struct NoopCustomNodeCallback;

#[async_trait::async_trait]
impl ZenCustomNodeCallback for NoopCustomNodeCallback {
    async fn handle(
        &self,
        _: ZenEngineHandlerRequest,
    ) -> Result<ZenEngineHandlerResponse, ZenError> {
        Err(ZenError::Zero)
    }
}

pub struct ZenCustomNodeCallbackWrapper(pub Box<dyn ZenCustomNodeCallback>);

impl CustomNodeAdapter for ZenCustomNodeCallbackWrapper {
    async fn handle(&self, request: CustomNodeRequest) -> NodeResult {
        let input = Arc::new(JsonBuffer(request.input.to_value()));
        let node = DecisionNode::from(request.node);

        let result = self
            .0
            .handle(ZenEngineHandlerRequest { input, node })
            .await
            .map_err(|err| anyhow!(err.details()))?;

        let output: Variable = result.output.to_variable();
        let trace_data: Option<Value> = result.trace_data.map(|trace| trace.to_value());

        Ok(NodeResponse { output, trace_data })
    }
}

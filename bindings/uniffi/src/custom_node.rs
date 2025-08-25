use crate::error::ZenError;
use crate::types::{DecisionNode, ZenEngineHandlerRequest, ZenEngineHandlerResponse};
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
        let input = request
            .input
            .try_into()
            .map_err(|err: ZenError| anyhow!(err))?;

        let node = DecisionNode::from(request.node);

        let result = self
            .0
            .handle(ZenEngineHandlerRequest { input, node })
            .await
            .map_err(|err| anyhow!(err.details()))?;

        let output: Variable = result
            .output
            .try_into()
            .map_err(|err: ZenError| anyhow!(err))?;

        let trace_data: Option<Variable> =
            result.trace_data.and_then(|trace| trace.try_into().ok());

        Ok(NodeResponse { output, trace_data })
    }
}

use crate::error::ZenError;
use crate::types::{DecisionNode, ZenEngineHandlerRequest, ZenEngineHandlerResponse};
use std::fmt::Debug;
use std::pin::Pin;
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::nodes::{NodeError, NodeResponse, NodeResult};
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

impl Debug for ZenCustomNodeCallbackWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ZenCustomNodeCallbackWrapper")
    }
}

impl CustomNodeAdapter for ZenCustomNodeCallbackWrapper {
    fn handle(&self, request: CustomNodeRequest) -> Pin<Box<dyn Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            let input = request
                .input
                .try_into()
                .map_err(|err: ZenError| NodeError {
                    trace: None,
                    node_id: request.node.id.clone(),
                    source: err.to_string().into(),
                })?;

            let node = DecisionNode::from(request.node.clone());

            let result = self
                .0
                .handle(ZenEngineHandlerRequest { input, node })
                .await
                .map_err(|err| NodeError {
                    trace: None,
                    node_id: request.node.id.clone(),
                    source: err.to_string().into(),
                })?;

            let output: Variable = result
                .output
                .try_into()
                .map_err(|err: ZenError| NodeError {
                    trace: None,
                    node_id: request.node.id.clone(),
                    source: err.to_string().into(),
                })?;

            let trace_data: Option<Variable> =
                result.trace_data.and_then(|trace| trace.try_into().ok());

            Ok(NodeResponse { output, trace_data })
        })
    }
}

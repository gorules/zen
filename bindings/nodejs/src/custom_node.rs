use crate::types::{ZenEngineHandlerRequest, ZenEngineHandlerResponse};
use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::ThreadsafeFunction;
use napi::Status;
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::nodes::{NodeError, NodeResponse, NodeResult};
use zen_engine::Variable;

type CustomNodeTsfn = Arc<
    ThreadsafeFunction<
        ZenEngineHandlerRequest,
        Promise<ZenEngineHandlerResponse>,
        ZenEngineHandlerRequest,
        Status,
        false,
    >,
>;

#[derive(Default)]
pub(crate) struct CustomNode {
    function: Option<CustomNodeTsfn>,
}

impl Debug for CustomNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "CustomNode")
    }
}

impl CustomNode {
    pub fn new(tsf: CustomNodeTsfn) -> Self {
        Self {
            function: Some(tsf),
        }
    }
}

impl CustomNodeAdapter for CustomNode {
    fn handle(&self, request: CustomNodeRequest) -> Pin<Box<dyn Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            let Some(function) = &self.function else {
                return Err(NodeError {
                    node_id: request.node.id.clone(),
                    trace: None,
                    source: "Custom function is undefined".into(),
                });
            };

            let node_data = crate::types::DecisionNode::from(request.node.clone());

            let promise: Promise<ZenEngineHandlerResponse> = function
                .clone()
                .call_async(ZenEngineHandlerRequest {
                    input: request.input.to_value(),
                    node: node_data,
                })
                .await
                .map_err(|err| NodeError {
                    node_id: request.node.id.clone(),
                    trace: None,
                    source: err.reason.to_string().into(),
                })?;

            let result = promise.await.map_err(|err| NodeError {
                node_id: request.node.id.clone(),
                trace: None,
                source: err.reason.to_string().into(),
            })?;

            Ok(NodeResponse {
                output: result.output.into(),
                trace_data: result.trace_data.map(Variable::from),
            })
        })
    }
}

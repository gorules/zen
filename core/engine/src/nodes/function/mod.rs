pub(crate) mod v1;
pub(crate) mod v2;

use crate::nodes::definition::NodeHandler;
use crate::nodes::function::v1::{FunctionV1NodeHandler, FunctionV1Trace};
use crate::nodes::function::v2::{FunctionV2NodeHandler, FunctionV2Trace};
use crate::nodes::result::NodeResult;
use crate::nodes::NodeContext;
use std::sync::Arc;
use zen_types::decision::{FunctionContent, FunctionNodeContent};
use zen_types::variable::Variable;

pub struct FunctionNodeHandler;

impl NodeHandler for FunctionNodeHandler {
    type NodeData = FunctionNodeContent;
    type TraceData = FunctionNodeTrace;

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        match &ctx.node {
            FunctionNodeContent::Version1(source) => {
                let v1_context = NodeContext::<Arc<str>, FunctionV1Trace> {
                    id: ctx.id.clone(),
                    name: ctx.name.clone(),
                    input: ctx.input.clone(),
                    extensions: ctx.extensions.clone(),
                    node: source.clone(),
                    trace: None,
                };

                FunctionV1NodeHandler.handle(v1_context)
            }
            FunctionNodeContent::Version2(content) => {
                let v2_context = NodeContext::<FunctionContent, FunctionV2Trace> {
                    id: ctx.id.clone(),
                    name: ctx.name.clone(),
                    input: ctx.input.clone(),
                    extensions: ctx.extensions.clone(),
                    node: content.clone(),
                    trace: None,
                };

                FunctionV2NodeHandler.handle(v2_context)
            }
        }
    }
}

pub type FunctionNodeTrace = Variable;

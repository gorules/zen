use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig};
use crate::nodes::{NodeContext, NodeContextExt, NodeHandler, NodeResult};
use crate::EvaluationError;
use std::ops::Deref;
use zen_types::decision::{DecisionNodeContent, TransformAttributes};
use zen_types::variable::{ToVariable, Variable};

pub struct DecisionNodeHandler;

pub type DecisionNodeData = DecisionNodeContent;
pub type DecisionNodeTrace = Variable;

impl NodeHandler for DecisionNodeHandler {
    type NodeData = DecisionNodeData;
    type TraceData = DecisionNodeTrace;

    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        Some(ctx.node.transform_attributes.clone())
    }

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let loader = ctx.extensions.loader();
        let sub_decision =
            ctx.try_block_on(async { loader.load(ctx.node.key.deref()).await.node_context(&ctx) })?;

        let mut decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: sub_decision,
            extensions: ctx.extensions.clone(),
            trace: ctx.has_trace(),
            iteration: ctx.iteration,
            max_depth: 10,
        })
        .node_context(&ctx)?;

        match decision_graph.evaluate(ctx.input.clone()) {
            Ok(result) => {
                ctx.trace(|trace| {
                    *trace = result.trace.to_variable();
                });

                ctx.success(result.result)
            }
            Err(err) => {
                if let EvaluationError::NodeError(node_error) = err.deref() {
                    ctx.trace(|trace| *trace = node_error.trace.to_variable());
                }

                ctx.error(err.to_string())
            }
        }
    }
}

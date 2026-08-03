use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig};
use crate::nodes::{NodeContext, NodeContextExt, NodeError, NodeHandler, NodeResult};
use crate::EvaluationError;
use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;
use zen_types::decision::{DecisionNodeContent, TransformAttributes};
use zen_types::variable::{ToVariable, Variable};

#[derive(Debug, Clone, Default)]
pub struct DecisionNodeHandler {
    decision_graph: Rc<RefCell<Option<DecisionGraph>>>,
}

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

    async fn after_transform_attributes(
        &self,
        _ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Result<(), NodeError> {
        if let Some(graph) = self.decision_graph.borrow_mut().as_mut() {
            graph.reset_graph();
        };

        Ok(())
    }

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let mut decision_graph_ref = self.decision_graph.borrow_mut();

        if decision_graph_ref.is_none() {
            let loader = ctx.extensions.loader();
            let sub_decision = loader.load(ctx.node.key.deref()).await.node_context(&ctx)?;
            let sub_kind = sub_decision.kind();
            let Some(sub_graph) = sub_decision.into_graph_arc() else {
                return ctx.error(format!(
                    "sub-decision '{}' is a {sub_kind}, expected graph",
                    ctx.node.key
                ));
            };

            let sub_graph =
                if sub_graph.compiled_cache.is_some() && sub_graph.resolved_schemas.is_some() {
                    sub_graph
                } else {
                    let mut owned = (*sub_graph).clone();
                    owned.compile();
                    let _ = owned.resolve_schemas(loader).await;
                    std::sync::Arc::new(owned)
                };

            let mut extensions = ctx.extensions.clone();
            extensions.compiled_cache = sub_graph.compiled_cache.clone();
            extensions.dt_indexes = sub_graph.dt_indexes.clone();
            extensions.validator_cache =
                std::sync::Arc::new(std::cell::OnceCell::from(sub_graph.validator_cache.clone()));

            let dg = DecisionGraph::try_new(DecisionGraphConfig {
                content: sub_graph,
                extensions,
                trace: ctx.config.trace,
                iteration: ctx.iteration + 1,
                max_depth: ctx.config.max_depth,
            })
            .node_context(&ctx)?;

            *decision_graph_ref = Some(dg);
        }

        let Some(decision_graph) = decision_graph_ref.as_mut() else {
            return ctx.error("Failed to initialize decision graph".to_string());
        };

        decision_graph.set_parent_nodes(ctx.nodes.clone());

        let evaluate_result = Box::pin(decision_graph.evaluate(ctx.input.clone())).await;
        match evaluate_result {
            Ok(result) => {
                ctx.trace(|trace| {
                    *trace = result
                        .trace
                        .and_then(|t| t.into_graph())
                        .as_ref()
                        .map(|m| m.to_variable())
                        .unwrap_or(Variable::Null);
                });

                ctx.success(result.result)
            }
            Err(err) => {
                if let EvaluationError::NodeError { trace, .. } = err.deref() {
                    ctx.trace(|t| *t = trace.to_variable());
                }

                ctx.error(err.to_string())
            }
        }
    }
}

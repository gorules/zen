use crate::compiled::host::graph::walker::StableDiDecisionGraph;
use crate::compiled::host::{
    NodeContext, NodeContextBase, NodeDataType, NodeHandler, NodeResult, TraceDataType,
};
use crate::decision_graph::graph::DecisionGraphResponse;
use crate::model::{DecisionNodeKind, GraphContent};
use crate::workspace::EvaluationResult;
use crate::{DecisionGraphValidationError, EvaluationError};
use ahash::{HashMap, HashMapExt};
use petgraph::algo::is_cyclic_directed;

#[derive(Debug)]
pub(crate) struct DecisionGraph;

impl DecisionGraph {
    pub(crate) fn build_graph(
        content: &GraphContent,
    ) -> Result<StableDiDecisionGraph, DecisionGraphValidationError> {
        let mut graph = StableDiDecisionGraph::new();
        let mut index_map = HashMap::with_capacity(content.nodes.len());

        for node in &content.nodes {
            let node_id = node.id.clone();
            let node_index = graph.add_node(node.clone());

            index_map.insert(node_id, node_index);
        }

        for edge in &content.edges {
            let source_index = index_map.get(&edge.source_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.source_id.to_string())
            })?;

            let target_index = index_map.get(&edge.target_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.target_id.to_string())
            })?;

            graph.add_edge(*source_index, *target_index, edge.clone());
        }

        Ok(graph)
    }

    pub(crate) fn validate_graph(
        graph: &StableDiDecisionGraph,
    ) -> Result<(), DecisionGraphValidationError> {
        let input_count = graph
            .node_weights()
            .filter(|w| matches!(w.kind, DecisionNodeKind::InputNode { .. }))
            .count();
        if input_count != 1 {
            return Err(DecisionGraphValidationError::InvalidInputCount(
                input_count as u32,
            ));
        }

        if is_cyclic_directed(graph) {
            return Err(DecisionGraphValidationError::CyclicGraph);
        }

        Ok(())
    }

    pub(crate) async fn handle<NodeData, TraceData, NodeHandlerType>(
        base_ctx: NodeContextBase,
        content: NodeData,
        handler: NodeHandlerType,
    ) -> NodeResult
    where
        TraceData: TraceDataType,
        NodeData: NodeDataType,
        NodeHandlerType: NodeHandler<NodeData = NodeData, TraceData = TraceData>,
    {
        handler
            .handle(NodeContext::<NodeData, TraceData>::from_base(
                base_ctx, content,
            ))
            .await
    }
}

pub(crate) struct Responses;

impl Responses {
    pub(crate) fn policy(result: EvaluationResult) -> DecisionGraphResponse {
        DecisionGraphResponse::from(result)
    }

    pub(crate) fn serialized(
        response: Result<DecisionGraphResponse, Box<EvaluationError>>,
    ) -> Result<serde_json::Value, serde_json::Value> {
        DecisionGraphResponse::serialized(response)
    }
}

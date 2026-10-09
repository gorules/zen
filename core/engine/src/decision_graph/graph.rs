use crate::decision_graph::walker::StableDiDecisionGraph;
use crate::engine::EvaluationTraceKind;
use crate::model::{DecisionNodeKind, GraphContent};
use crate::nodes::{
    NodeContext, NodeContextBase, NodeDataType, NodeHandler, NodeHandlerExtensions, NodeResult,
    TraceDataType,
};
use crate::{DecisionGraphTrace, DecisionGraphValidationError};
use ahash::{HashMap, HashMapExt};
use petgraph::algo::is_cyclic_directed;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use std::ops::Deref;
use std::sync::Arc;
use zen_expression::variable::{ToVariable, Variable};

#[derive(Debug)]
pub struct DecisionGraph {
    pub(super) initial_graph: StableDiDecisionGraph,
    pub(super) graph: StableDiDecisionGraph,
    pub(super) config: DecisionGraphConfig,
    pub(super) parent_nodes: Option<Variable>,
}

#[derive(Debug)]
pub struct DecisionGraphConfig {
    pub content: Arc<GraphContent>,
    pub trace: bool,
    pub iteration: u8,
    pub max_depth: u8,
    pub extensions: NodeHandlerExtensions,
}

impl DecisionGraph {
    pub fn try_new(config: DecisionGraphConfig) -> Result<Self, DecisionGraphValidationError> {
        let graph = Self::build_graph(config.content.deref())?;
        Ok(Self {
            initial_graph: graph.clone(),
            graph,
            config,
            parent_nodes: None,
        })
    }

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

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        Self::validate_graph(&self.graph)
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EvaluationTrace {
    Graph(HashMap<Arc<str>, DecisionGraphTrace>),
    Policy(crate::policy::Trace),
}

impl EvaluationTrace {
    pub fn as_graph(&self) -> Option<&HashMap<Arc<str>, DecisionGraphTrace>> {
        match self {
            Self::Graph(m) => Some(m),
            Self::Policy(_) => None,
        }
    }

    pub fn into_graph(self) -> Option<HashMap<Arc<str>, DecisionGraphTrace>> {
        match self {
            Self::Graph(m) => Some(m),
            Self::Policy(_) => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphResponse {
    pub performance: String,
    pub result: Variable,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<EvaluationTrace>,
}

impl DecisionGraphResponse {
    pub fn serialize_with_mode<S>(
        &self,
        serializer: S,
        mode: EvaluationTraceKind,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("performance", &self.performance)?;
        map.serialize_entry("result", &self.result)?;
        if let Some(trace) = &self.trace {
            match trace {
                EvaluationTrace::Graph(graph_trace) => {
                    map.serialize_entry(
                        "trace",
                        &mode.serialize_trace(&graph_trace.to_variable()),
                    )?;
                }
                EvaluationTrace::Policy(policy_trace) => match mode {
                    EvaluationTraceKind::String | EvaluationTraceKind::ReferenceString => {
                        map.serialize_entry(
                            "trace",
                            &serde_json::to_string(policy_trace).unwrap_or_default(),
                        )?;
                    }
                    _ => {
                        map.serialize_entry("trace", policy_trace)?;
                    }
                },
            }
        }

        map.end()
    }
}

impl DecisionGraph {
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
            .handle(NodeContext::<NodeData, TraceData>::from_base(base_ctx, content))
            .await
    }
}

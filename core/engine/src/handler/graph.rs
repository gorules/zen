use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use petgraph::algo::is_cyclic_directed;
use rquickjs::Runtime;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::handler::custom_node_adapter::{CustomNodeAdapter, CustomNodeRequest};
use crate::handler::decision::DecisionHandler;
use crate::handler::expression::ExpressionHandler;
use crate::handler::function::runtime::create_runtime;
use crate::handler::function::FunctionHandler;
use crate::handler::node::NodeRequest;
use crate::handler::table::zen::DecisionTableHandler;
use crate::handler::traversal::{GraphWalker, StableDiDecisionGraph};
use crate::loader::DecisionLoader;
use crate::model::{DecisionContent, DecisionNodeKind};
use crate::{EvaluationError, NodeError};

pub struct DecisionGraph<'a, L: DecisionLoader, A: CustomNodeAdapter> {
    graph: StableDiDecisionGraph<'a>,
    adapter: Arc<A>,
    loader: Arc<L>,
    trace: bool,
    max_depth: u8,
    iteration: u8,
    runtime: Option<Runtime>,
}

pub struct DecisionGraphConfig<'a, L: DecisionLoader, A: CustomNodeAdapter> {
    pub loader: Arc<L>,
    pub adapter: Arc<A>,
    pub content: &'a DecisionContent,
    pub trace: bool,
    pub iteration: u8,
    pub max_depth: u8,
}

impl<'a, L: DecisionLoader, A: CustomNodeAdapter> DecisionGraph<'a, L, A> {
    pub fn try_new(
        config: DecisionGraphConfig<'a, L, A>,
    ) -> Result<Self, DecisionGraphValidationError> {
        let content = config.content;
        let mut graph = StableDiDecisionGraph::new();
        let mut index_map = HashMap::new();

        for node in &content.nodes {
            let node_id = node.id.clone();
            let node_index = graph.add_node(node);

            index_map.insert(node_id, node_index);
        }

        for (_, edge) in content.edges.iter().enumerate() {
            let source_index = index_map.get(&edge.source_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.source_id.to_string())
            })?;

            let target_index = index_map.get(&edge.target_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.target_id.to_string())
            })?;

            graph.add_edge(source_index.clone(), target_index.clone(), edge);
        }

        Ok(Self {
            graph,
            iteration: config.iteration,
            trace: config.trace,
            loader: config.loader,
            adapter: config.adapter,
            max_depth: config.max_depth,
            runtime: None,
        })
    }

    pub(crate) fn with_runtime(mut self, runtime: Option<Runtime>) -> Self {
        self.runtime = runtime;
        self
    }

    fn get_or_insert_runtime(&mut self) -> anyhow::Result<Runtime> {
        if let Some(runtime) = &self.runtime {
            return Ok(runtime.clone());
        }

        let runtime = create_runtime()?;
        self.runtime.replace(runtime.clone());

        Ok(runtime)
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let input_count = self.node_kind_count(DecisionNodeKind::InputNode);
        if input_count != 1 {
            return Err(DecisionGraphValidationError::InvalidInputCount(
                input_count as u32,
            ));
        }

        let output_count = self.node_kind_count(DecisionNodeKind::OutputNode);
        if output_count < 1 {
            return Err(DecisionGraphValidationError::InvalidOutputCount(
                output_count as u32,
            ));
        }

        if is_cyclic_directed(&self.graph) {
            return Err(DecisionGraphValidationError::CyclicGraph);
        }

        Ok(())
    }

    fn node_kind_count(&self, kind: DecisionNodeKind) -> usize {
        self.graph
            .node_weights()
            .filter(|weight| weight.kind == kind)
            .count()
    }

    pub async fn evaluate(&mut self, context: &Value) -> Result<DecisionGraphResponse, NodeError> {
        let root_start = Instant::now();

        self.validate().map_err(|e| NodeError {
            node_id: "".to_string(),
            source: anyhow!(e),
        })?;

        if self.iteration >= self.max_depth {
            return Err(NodeError {
                node_id: "".to_string(),
                source: anyhow!(EvaluationError::DepthLimitExceeded),
            });
        }

        let mut walker = GraphWalker::new(&self.graph);
        let mut node_traces = self.trace.then(|| HashMap::default());

        while let Some((nid, walker_metadata)) = walker.next(&mut self.graph) {
            if let Some(_) = walker.get_node_data(nid) {
                continue;
            }

            let node = self.graph[nid];
            let start = Instant::now();

            macro_rules! trace {
                ($data: tt) => {
                    if let Some(nt) = &mut node_traces {
                        nt.insert(node.id.clone(), DecisionGraphTrace $data);
                    };
                };
            }

            match &node.kind {
                DecisionNodeKind::InputNode => {
                    walker.set_node_data(nid, context.clone());
                    trace!({
                        input: Value::Null,
                        output: Value::Null,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: None,
                        trace_data: None,
                    });
                }
                DecisionNodeKind::OutputNode => {
                    trace!({
                        input: Value::Null,
                        output: Value::Null,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: None,
                        trace_data: None,
                    });

                    return Ok(DecisionGraphResponse {
                        result: walker.incoming_node_data(&self.graph, nid, false),
                        performance: format!("{:?}", root_start.elapsed()),
                        trace: node_traces,
                    });
                }
                DecisionNodeKind::SwitchNode { .. } => {
                    let input_data = walker.incoming_node_data(&self.graph, nid, false);
                    walker.set_node_data(nid, input_data.clone());

                    trace!({
                        input: input_data.clone(),
                        output: input_data,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: Some(walker_metadata),
                    });
                }
                DecisionNodeKind::FunctionNode { .. } => {
                    let runtime = self.get_or_insert_runtime().map_err(|e| NodeError {
                        source: e.into(),
                        node_id: node.id.clone(),
                    })?;

                    let mut node_request = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };
                    let mut res = FunctionHandler::new(self.trace, runtime)
                        .handle(&node_request)
                        .await
                        .map_err(|e| NodeError {
                            source: e.into(),
                            node_id: node.id.clone(),
                        })?;

                    trim_nodes(&mut node_request.input);
                    trim_nodes(&mut res.output);

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::DecisionNode { .. } => {
                    let mut node_request = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let mut res = DecisionHandler::new(
                        self.trace,
                        self.max_depth,
                        self.loader.clone(),
                        self.adapter.clone(),
                        self.runtime.clone(),
                    )
                    .handle(&node_request)
                    .await
                    .map_err(|e| NodeError {
                        source: e.into(),
                        node_id: node.id.to_string(),
                    })?;

                    trim_nodes(&mut node_request.input);
                    trim_nodes(&mut res.output);

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::DecisionTableNode { .. } => {
                    let mut node_request = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let mut res = DecisionTableHandler::new(self.trace)
                        .handle(&node_request)
                        .await
                        .map_err(|e| NodeError {
                            node_id: node.id.clone(),
                            source: e.into(),
                        })?;

                    trim_nodes(&mut node_request.input);
                    trim_nodes(&mut res.output);

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::ExpressionNode { .. } => {
                    let mut node_request = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let mut res = ExpressionHandler::new(self.trace)
                        .handle(&node_request)
                        .await
                        .map_err(|e| NodeError {
                            node_id: node.id.clone(),
                            source: e.into(),
                        })?;

                    trim_nodes(&mut node_request.input);
                    trim_nodes(&mut res.output);

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::CustomNode { .. } => {
                    let mut node_request = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let mut res = self
                        .adapter
                        .handle(CustomNodeRequest::try_from(&node_request).unwrap())
                        .await
                        .map_err(|e| NodeError {
                            node_id: node.id.clone(),
                            source: e.into(),
                        })?;

                    trim_nodes(&mut node_request.input);
                    trim_nodes(&mut res.output);

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
            }
        }

        Err(NodeError {
            node_id: "".to_string(),
            source: anyhow!("Graph did not halt. Missing output node."),
        })
    }
}

fn trim_nodes(val: &mut Value) {
    if let Some(obj) = val.as_object_mut() {
        obj.remove("$nodes");
    }
}

#[derive(Debug, Error)]
pub enum DecisionGraphValidationError {
    #[error("Invalid input node count: {0}")]
    InvalidInputCount(u32),

    #[error("Invalid output node count: {0}")]
    InvalidOutputCount(u32),

    #[error("Cyclic graph detected")]
    CyclicGraph,

    #[error("Missing node")]
    MissingNode(String),
}

impl Serialize for DecisionGraphValidationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;

        match &self {
            DecisionGraphValidationError::InvalidInputCount(count) => {
                map.serialize_entry("type", "invalidInputCount")?;
                map.serialize_entry("nodeCount", count)?;
            }
            DecisionGraphValidationError::InvalidOutputCount(count) => {
                map.serialize_entry("type", "invalidOutputCount")?;
                map.serialize_entry("nodeCount", count)?;
            }
            DecisionGraphValidationError::MissingNode(node_id) => {
                map.serialize_entry("type", "missingNode")?;
                map.serialize_entry("nodeId", node_id)?;
            }
            DecisionGraphValidationError::CyclicGraph => {
                map.serialize_entry("type", "cyclicGraph")?;
            }
        }

        map.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphResponse {
    pub performance: String,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<HashMap<String, DecisionGraphTrace>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphTrace {
    pub input: Value,
    pub output: Value,
    pub name: String,
    pub id: String,
    pub performance: Option<String>,
    pub trace_data: Option<Value>,
}

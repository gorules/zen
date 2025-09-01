use crate::decision_graph::walker::{GraphWalker, NodeData, StableDiDecisionGraph};
use crate::engine::EvaluationTraceKind;
use crate::model::{DecisionContent, DecisionNodeKind};
use crate::nodes::custom::{CustomNodeData, CustomNodeHandler, CustomNodeTrace};
use crate::nodes::decision::{DecisionNodeData, DecisionNodeHandler, DecisionNodeTrace};
use crate::nodes::decision_table::{
    DecisionTableNodeData, DecisionTableNodeHandler, DecisionTableNodeTrace,
};
use crate::nodes::expression::{ExpressionNodeData, ExpressionNodeHandler, ExpressionNodeTrace};
use crate::nodes::function::{FunctionNodeData, FunctionNodeHandler, FunctionNodeTrace};
use crate::nodes::input::{InputNodeData, InputNodeHandler, InputNodeTrace};
use crate::nodes::output::{OutputNodeData, OutputNodeHandler, OutputNodeTrace};
use crate::nodes::{
    NodeContext, NodeContextBase, NodeHandler, NodeHandlerExtensions, NodeResponse,
};
use crate::EvaluationError;
use ahash::{HashMap, HashMapExt};
use petgraph::algo::is_cyclic_directed;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use zen_expression::variable::{ToVariable, Variable};

pub struct DecisionGraph {
    initial_graph: StableDiDecisionGraph,
    graph: StableDiDecisionGraph,
    trace: bool,
    max_depth: u8,
    iteration: u8,
    extensions: NodeHandlerExtensions,
}

pub struct DecisionGraphConfig {
    pub content: Arc<DecisionContent>,
    pub trace: bool,
    pub iteration: u8,
    pub max_depth: u8,
    pub extensions: NodeHandlerExtensions,
}

impl DecisionGraph {
    pub fn try_new(config: DecisionGraphConfig) -> Result<Self, DecisionGraphValidationError> {
        let content = config.content;
        let mut graph = StableDiDecisionGraph::new();
        let mut index_map = HashMap::new();

        for node in &content.nodes {
            let node_id = node.id.clone();
            let node_index = graph.add_node(node.clone());

            index_map.insert(node_id, node_index);
        }

        for (_, edge) in content.edges.iter().enumerate() {
            let source_index = index_map.get(&edge.source_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.source_id.to_string())
            })?;

            let target_index = index_map.get(&edge.target_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.target_id.to_string())
            })?;

            graph.add_edge(source_index.clone(), target_index.clone(), edge.clone());
        }

        Ok(Self {
            initial_graph: graph.clone(),
            graph,
            iteration: config.iteration,
            trace: config.trace,
            max_depth: config.max_depth,
            extensions: config.extensions,
        })
    }

    pub(crate) fn reset_graph(&mut self) {
        self.graph = self.initial_graph.clone();
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let input_count = self.input_node_count();
        if input_count != 1 {
            return Err(DecisionGraphValidationError::InvalidInputCount(
                input_count as u32,
            ));
        }

        if is_cyclic_directed(&self.graph) {
            return Err(DecisionGraphValidationError::CyclicGraph);
        }

        Ok(())
    }

    fn input_node_count(&self) -> usize {
        self.graph
            .node_weights()
            .filter(|weight| matches!(weight.kind, DecisionNodeKind::InputNode { content: _ }))
            .count()
    }

    pub fn evaluate(
        &mut self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let root_start = Instant::now();

        self.validate()?;

        if self.iteration >= self.max_depth {
            return Err(Box::new(EvaluationError::DepthLimitExceeded));
        }

        let mut walker = GraphWalker::new(&self.graph);
        let mut node_traces = self.trace.then(|| HashMap::default());

        while let Some(nid) = walker.next(
            &mut self.graph,
            self.trace.then_some(|mut trace: DecisionGraphTrace| {
                if let Some(nt) = &mut node_traces {
                    trace.order = nt.len() as u32;
                    nt.insert(trace.id.clone(), trace);
                };
            }),
        ) {
            if let Some(_) = walker.get_node_data(nid) {
                continue;
            }

            let mut terminate = false;
            let node = (&self.graph[nid]).clone();
            let start = Instant::now();
            let incoming_data = walker.incoming_node_data(&self.graph, nid, true);

            let mut base_ctx = NodeContextBase {
                id: node.id.clone(),
                name: node.name.clone(),
                input: incoming_data.clone(),
                extensions: self.extensions.clone(),
                iteration: self.iteration,
                trace: self.trace,
            };

            let node_execution = match &node.kind {
                DecisionNodeKind::InputNode { content } => {
                    base_ctx.input = context.clone();
                    let ctx = NodeContext::<InputNodeData, InputNodeTrace>::from_base(
                        base_ctx,
                        content.clone(),
                    );

                    InputNodeHandler.handle(ctx)
                }
                DecisionNodeKind::OutputNode { content } => {
                    terminate = true;
                    let ctx = NodeContext::<OutputNodeData, OutputNodeTrace>::from_base(
                        base_ctx,
                        content.clone(),
                    );

                    OutputNodeHandler.handle(ctx)
                }
                DecisionNodeKind::SwitchNode { .. } => {
                    let input_data = walker.incoming_node_data(&self.graph, nid, false);

                    // walker.set_node_data(nid, input_data);
                    Ok(NodeResponse {
                        output: Variable::Null,
                        trace_data: None,
                    })
                }
                DecisionNodeKind::FunctionNode { content } => {
                    let ctx = NodeContext::<FunctionNodeData, FunctionNodeTrace>::from_base(
                        base_ctx,
                        content.clone(),
                    );

                    FunctionNodeHandler.handle(ctx)
                }
                DecisionNodeKind::DecisionNode { content } => {
                    let ctx = NodeContext::<DecisionNodeData, DecisionNodeTrace>::from_base(
                        base_ctx,
                        content.clone(),
                    );

                    DecisionNodeHandler.handle(ctx)
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    let ctx =
                        NodeContext::<DecisionTableNodeData, DecisionTableNodeTrace>::from_base(
                            base_ctx,
                            content.clone(),
                        );

                    DecisionTableNodeHandler.handle(ctx)
                }
                DecisionNodeKind::ExpressionNode { content } => {
                    let ctx = NodeContext::<ExpressionNodeData, ExpressionNodeTrace>::from_base(
                        base_ctx,
                        content.clone(),
                    );

                    ExpressionNodeHandler.handle(ctx)
                }
                DecisionNodeKind::CustomNode { content } => {
                    let ctx = NodeContext::<CustomNodeData, CustomNodeTrace>::from_base(
                        base_ctx,
                        content.clone(),
                    );

                    CustomNodeHandler.handle(ctx)
                }
            };

            if let Some(nt) = &mut node_traces {
                let trace = match &node_execution {
                    Ok(ok) => DecisionGraphTrace {
                        id: node.id.clone(),
                        name: node.name.clone(),
                        input: incoming_data,
                        order: nt.len() as u32,
                        output: ok.output.clone(),
                        trace_data: ok.trace_data.clone(),
                        performance: Some(Arc::from(format!("{:.1?}", start.elapsed()))),
                    },
                    Err(err) => DecisionGraphTrace {
                        id: node.id.clone(),
                        name: node.name.clone(),
                        input: incoming_data,
                        order: nt.len() as u32,
                        output: Variable::Null,
                        trace_data: err.trace.clone(),
                        performance: Some(Arc::from(format!("{:.1?}", start.elapsed()))),
                    },
                };

                nt.insert(node.id.clone(), trace);
            }

            walker.set_node_data(
                nid,
                NodeData {
                    name: Rc::from(node.name.deref()),
                    data: node_execution?.output,
                },
            );

            if terminate {
                break;
            }
        }

        Ok(DecisionGraphResponse {
            result: walker.ending_variables(&self.graph),
            performance: format!("{:.1?}", root_start.elapsed()),
            trace: node_traces,
        })
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
    pub result: Variable,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<HashMap<Arc<str>, DecisionGraphTrace>>,
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
            map.serialize_entry("trace", &mode.serialize_trace(&trace.to_variable()))?;
        }

        map.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToVariable)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphTrace {
    pub input: Variable,
    pub output: Variable,
    pub name: Arc<str>,
    pub id: Arc<str>,
    pub performance: Option<Arc<str>>,
    pub trace_data: Option<Variable>,
    pub order: u32,
}

pub(crate) fn error_trace(trace: &Option<HashMap<String, DecisionGraphTrace>>) -> Option<Variable> {
    trace.as_ref().map(|s| {
        s.values().for_each(|v| {
            v.input.dot_remove("$nodes");
            v.output.dot_remove("$nodes");
        });

        s.to_variable()
    })
}

fn create_validator_cache_key(content: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

use crate::decision_graph::tracer::NodeTracer;
use crate::decision_graph::walker::{GraphWalker, NodeData, StableDiDecisionGraph};
use crate::engine::EvaluationTraceKind;
use crate::model::{DecisionContent, DecisionNodeKind};
use crate::nodes::custom::CustomNodeHandler;
use crate::nodes::decision::DecisionNodeHandler;
use crate::nodes::decision_table::DecisionTableNodeHandler;
use crate::nodes::expression::ExpressionNodeHandler;
use crate::nodes::function::FunctionNodeHandler;
use crate::nodes::input::InputNodeHandler;
use crate::nodes::output::OutputNodeHandler;
use crate::nodes::transform_attributes::TransformAttributesExecution;
use crate::nodes::{
    NodeContext, NodeContextBase, NodeContextConfig, NodeDataType, NodeHandler,
    NodeHandlerExtensions, NodeResponse, NodeResult, TraceDataType,
};
use crate::{DecisionGraphTrace, DecisionGraphValidationError, EvaluationError};
use ahash::{HashMap, HashMapExt};
use petgraph::algo::is_cyclic_directed;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::variable::{ToVariable, Variable};
use zen_types::decision::DecisionNode;

#[derive(Debug)]
pub struct DecisionGraph {
    initial_graph: StableDiDecisionGraph,
    graph: StableDiDecisionGraph,
    config: DecisionGraphConfig,
}

#[derive(Debug)]
pub struct DecisionGraphConfig {
    pub content: Arc<DecisionContent>,
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
        })
    }

    fn build_graph(
        content: &DecisionContent,
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

    pub(crate) fn reset_graph(&mut self) {
        self.graph = self.initial_graph.clone();
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let input_count = self
            .graph
            .node_weights()
            .filter(|w| matches!(w.kind, DecisionNodeKind::InputNode { .. }))
            .count();
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

    fn build_node_context(&self, node: &DecisionNode, input: Variable) -> NodeContextBase {
        NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            extensions: self.config.extensions.clone(),
            iteration: self.config.iteration,
            config: NodeContextConfig {
                max_depth: self.config.max_depth,
                trace: self.config.trace,
                ..Default::default()
            },
        }
    }

    pub async fn evaluate(
        &mut self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let root_start = Instant::now();

        self.validate()?;
        if self.config.iteration >= self.config.max_depth {
            return Err(Box::new(EvaluationError::DepthLimitExceeded));
        }

        let mut walker = GraphWalker::new(&self.graph);
        let mut tracer = NodeTracer::new(self.config.trace);

        while let Some(nid) = walker.next(&mut self.graph, tracer.trace_callback()) {
            if let Some(_) = walker.get_node_data(nid) {
                continue;
            }

            let node = &self.graph[nid];
            let start = Instant::now();
            let (input, input_trace) = walker.incoming_node_data(&self.graph, nid, true);
            let mut base_ctx = self.build_node_context(node.deref(), input);

            let node_execution = match &node.kind {
                DecisionNodeKind::InputNode { content } => {
                    base_ctx.input = context.clone();
                    handle_node(base_ctx, content.clone(), InputNodeHandler).await
                }
                DecisionNodeKind::OutputNode { content } => {
                    handle_node(base_ctx, content.clone(), OutputNodeHandler).await
                }
                DecisionNodeKind::SwitchNode { .. } => Ok(NodeResponse {
                    output: input_trace.clone(),
                    trace_data: None,
                }),
                DecisionNodeKind::FunctionNode { content } => {
                    handle_node(base_ctx, content.clone(), FunctionNodeHandler).await
                }
                DecisionNodeKind::DecisionNode { content } => {
                    handle_node(base_ctx, content.clone(), DecisionNodeHandler::default()).await
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    handle_node(base_ctx, content.clone(), DecisionTableNodeHandler).await
                }
                DecisionNodeKind::ExpressionNode { content } => {
                    handle_node(base_ctx, content.clone(), ExpressionNodeHandler).await
                }
                DecisionNodeKind::CustomNode { content } => {
                    handle_node(base_ctx, content.clone(), CustomNodeHandler).await
                }
            };

            tracer.record_execution(node.deref(), input_trace, &node_execution, start.elapsed());

            let output = node_execution?.output;
            output.dot_remove("$nodes");

            walker.set_node_data(
                nid,
                NodeData {
                    name: Rc::from(node.name.deref()),
                    data: output,
                },
            );

            // Terminate once Output node is reached
            if matches!(node.kind, DecisionNodeKind::OutputNode { .. }) {
                break;
            }
        }

        Ok(DecisionGraphResponse {
            result: walker.ending_variables(&self.graph),
            performance: format!("{:.1?}", root_start.elapsed()),
            trace: tracer.into_traces(),
        })
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

async fn handle_node<NodeData, TraceData, NodeHandlerType>(
    base_ctx: NodeContextBase,
    content: NodeData,
    handler: NodeHandlerType,
) -> NodeResult
where
    TraceData: TraceDataType,
    NodeData: NodeDataType,
    NodeHandlerType: NodeHandler<NodeData = NodeData, TraceData = TraceData>,
{
    let ctx = NodeContext::<NodeData, TraceData>::from_base(base_ctx.clone(), content);
    if let Some(transform_attributes) = handler.transform_attributes(&ctx) {
        return transform_attributes
            .run_with(base_ctx, move |input, has_more| {
                let handler = handler.clone();
                let mut new_ctx = ctx.clone();
                new_ctx.input = input;

                async move {
                    match has_more {
                        false => handler.handle(new_ctx).await,
                        true => {
                            let result = handler.handle(new_ctx.clone()).await;
                            handler.after_transform_attributes(&new_ctx).await?;
                            result
                        }
                    }
                }
            })
            .await;
    }

    handler.handle(ctx).await
}

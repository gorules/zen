use crate::decision_graph::cleaner::VariableCleaner;
use crate::decision_graph::graph::{DecisionGraph, DecisionGraphResponse, EvaluationTrace};
use crate::model::GraphContent;
use crate::nodes::NodeHandlerExtensions;
use crate::DecisionGraphValidationError;
use crate::decision_graph::tracer::{NodeTracer, SwitchStatementTrace, SwitchStatementTraceRow};
use crate::decision_graph::walker::{GraphWalker, NodeData, StableDiDecisionGraph, WalkStep};
use crate::model::{DecisionNodeKind, SwitchStatement, SwitchStatementHitPolicy};
use crate::nodes::custom::CustomNodeHandler;
use crate::nodes::decision::DecisionNodeHandler;
use crate::nodes::decision_table::DecisionTableNodeHandler;
use crate::nodes::expression::ExpressionNodeHandler;
use crate::nodes::function::FunctionNodeHandler;
use crate::nodes::input::InputNodeHandler;
use crate::nodes::output::OutputNodeHandler;
use crate::nodes::transform_attributes::TransformAttributesExecution;
use crate::nodes::{
    NodeContext, NodeContextBase, NodeContextConfig, NodeDataType, NodeHandler, NodeResponse,
    NodeResult, TraceDataType,
};
use crate::{DecisionGraphTrace, EvaluationError};
use petgraph::matrix_graph::Zero;
use petgraph::prelude::NodeIndex;
use petgraph::Incoming;
use std::cell::RefCell;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::variable::{ToVariable, Variable};
use zen_expression::Isolate;
use zen_types::decision::{DecisionNode, InputNodeContent, OutputNodeContent};

impl GraphWalker {
    pub fn get_node_data(&self, node_id: NodeIndex) -> Option<Variable> {
        Some(self.node_data.get(&node_id)?.data.clone())
    }

    pub fn ending_variables(&self, g: &StableDiDecisionGraph) -> Variable {
        Self::merge_ending(
            self.ending_nodes(g)
                .into_iter()
                .filter_map(|nid| self.node_data.get(&nid).map(|nd| &nd.data)),
        )
    }

    pub fn get_all_node_data(&self) -> Variable {
        let node_values = self
            .node_data
            .iter()
            .filter_map(|(_, nd)| {
                let value = nd.nodes_view.clone().unwrap_or_else(|| nd.data.clone());
                Some((nd.name.clone(), value))
            })
            .collect();

        Variable::from_object(node_values)
    }

    pub fn nodes_context(&self) -> Option<Variable> {
        self.nodes_in_context.then(|| self.get_all_node_data())
    }

    pub fn incoming_node_data(
        &self,
        g: &StableDiDecisionGraph,
        node_id: NodeIndex,
    ) -> (Variable, Variable) {
        let value = self.merge_node_data(g.neighbors_directed(node_id, Incoming));

        (value.clone(), value)
    }

    pub fn merge_node_data<I>(&self, iter: I) -> Variable
    where
        I: Iterator<Item = NodeIndex>,
    {
        Self::merge_values(iter.filter_map(|nid| self.node_data.get(&nid).map(|nd| &nd.data)))
    }

    pub fn next<F: FnMut(DecisionGraphTrace)>(
        &mut self,
        g: &mut StableDiDecisionGraph,
        mut on_trace: Option<F>,
    ) -> Option<NodeIndex> {
        let start = Instant::now();
        let step = self.next_with(g, |walker, g, nid, node, content| {
            let (input, input_trace) = walker.incoming_node_data(g, nid);
            let mut isolate = Isolate::with_environment(input);
            if let Some(nodes) = walker.nodes_context() {
                isolate.set_local(Variable::nodes_key(), nodes);
            }

            let mut statement_iter = content.statements.iter();
            let valid_statements: Vec<SwitchStatementTraceRow> = match content.hit_policy {
                SwitchStatementHitPolicy::First => statement_iter
                    .find(|&s| Self::switch_statement_evaluate(&mut isolate, &s))
                    .into_iter()
                    .cloned()
                    .map(SwitchStatementTraceRow::from)
                    .collect(),
                SwitchStatementHitPolicy::Collect => statement_iter
                    .filter(|&s| Self::switch_statement_evaluate(&mut isolate, &s))
                    .cloned()
                    .map(SwitchStatementTraceRow::from)
                    .collect(),
            };

            if let Some(on_trace) = &mut on_trace {
                let output = input_trace.depth_clone(1);

                on_trace(DecisionGraphTrace {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    input: input_trace,
                    output,
                    order: 0,
                    performance: Some(Arc::from(format!("{:.1?}", start.elapsed()))),
                    trace_data: Some(
                        SwitchStatementTrace {
                            statements: valid_statements.clone(),
                        }
                        .to_variable(),
                    ),
                });
            }

            Some(valid_statements.into_iter().map(|s| s.id).collect())
        });

        match step {
            WalkStep::Node(nid) => Some(nid),
            WalkStep::Done | WalkStep::Pending => None,
        }
    }

    fn switch_statement_evaluate(isolate: &mut Isolate, switch_statement: &SwitchStatement) -> bool {
        if switch_statement.condition.is_empty() {
            return true;
        }

        isolate
            .run_standard(switch_statement.condition.deref())
            .map_or(false, |v| v.as_bool().unwrap_or(false))
    }
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

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        Self::validate_graph(&self.graph)
    }

    pub(crate) fn set_parent_nodes(&mut self, nodes: Option<Variable>) {
        self.parent_nodes = nodes;
    }

    pub(crate) fn reset_graph(&mut self) {
        self.graph = self.initial_graph.clone();
    }

    fn build_node_context(
        &self,
        node: &DecisionNode,
        input: Variable,
        nodes: Option<Variable>,
    ) -> NodeContextBase {
        NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            nodes,
            extensions: self.config.extensions.clone(),
            iteration: self.config.iteration,
            trace: match self.config.trace {
                true => Some(RefCell::new(Variable::Null)),
                false => None,
            },
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
            let start = self.config.trace.then(Instant::now);
            let (input, input_trace) = walker.incoming_node_data(&self.graph, nid);
            let mut base_ctx = self.build_node_context(node.deref(), input, walker.nodes_context());

            let node_execution = match &node.kind {
                DecisionNodeKind::InputNode { content } if content.target.is_some() => {
                    match self
                        .config
                        .content
                        .request_input(self.config.extensions.loader(), content, &context)
                        .await
                    {
                        Err(message) => base_ctx.error(message),
                        Ok(prepared) => {
                            base_ctx.input = prepared;
                            Self::handle_node(base_ctx, content.clone(), InputNodeHandler).await
                        }
                    }
                }
                DecisionNodeKind::InputNode { content } => {
                    base_ctx.input = context.clone();
                    match self
                        .config
                        .content
                        .validation_schema(
                            self.config.extensions.loader(),
                            &node.id,
                            content.schema.as_deref(),
                        )
                        .await
                    {
                        Err(message) => base_ctx.error(message),
                        Ok(None) => Self::handle_node(base_ctx, content.clone(), InputNodeHandler).await,
                        Ok(Some((schema, salt))) => {
                            base_ctx.config.validation_salt = salt;
                            let resolved = InputNodeContent {
                                schema: Some(schema),
                                target: content.target.clone(),
                            };
                            Self::handle_node(base_ctx, resolved, InputNodeHandler).await
                        }
                    }
                }
                DecisionNodeKind::OutputNode { content } => {
                    match self
                        .config
                        .content
                        .validation_schema(
                            self.config.extensions.loader(),
                            &node.id,
                            content.schema.as_deref(),
                        )
                        .await
                    {
                        Err(message) => base_ctx.error(message),
                        Ok(None) => Self::handle_node(base_ctx, content.clone(), OutputNodeHandler).await,
                        Ok(Some((schema, salt))) => {
                            base_ctx.config.validation_salt = salt;
                            let resolved = OutputNodeContent {
                                schema: Some(schema),
                            };
                            Self::handle_node(base_ctx, resolved, OutputNodeHandler).await
                        }
                    }
                }
                DecisionNodeKind::SwitchNode { .. } => Ok(NodeResponse {
                    output: input_trace.clone(),
                    trace_data: None,
                }),
                DecisionNodeKind::FunctionNode { content } => {
                    Self::handle_node(base_ctx, content.clone(), FunctionNodeHandler).await
                }
                DecisionNodeKind::DecisionNode { content } => {
                    Self::handle_node(base_ctx, content.clone(), DecisionNodeHandler::default()).await
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    Self::handle_node(base_ctx, content.clone(), DecisionTableNodeHandler).await
                }
                DecisionNodeKind::ExpressionNode { content } => {
                    Self::handle_node(base_ctx, content.clone(), ExpressionNodeHandler).await
                }
                DecisionNodeKind::CustomNode { content } => {
                    Self::handle_node(base_ctx, content.clone(), CustomNodeHandler).await
                }
            };

            tracer.record_execution(
                node.deref(),
                input_trace,
                &node_execution,
                start.map(|s| s.elapsed()).unwrap_or_default(),
            );

            let output = match node_execution {
                Ok(ok) => ok.output,
                Err(err) => {
                    let trace = tracer.into_traces();
                    if let Some(t) = &trace {
                        let mut cleaner = VariableCleaner::new();
                        t.values().for_each(|v| {
                            cleaner.clean(&v.input);
                            cleaner.clean(&v.output);
                            if let Some(td) = &v.trace_data {
                                cleaner.clean(td);
                            }
                        })
                    }

                    return Err(Box::new(EvaluationError::NodeError {
                        node_id: err.node_id,
                        source: err.source,
                        trace: trace.map(|t| t.to_variable()),
                    }));
                }
            };

            let nodes_view = match (&node.kind, &self.parent_nodes) {
                (DecisionNodeKind::InputNode { .. }, Some(parent_nodes)) => {
                    let view = output.depth_clone(1);
                    view.dot_insert(Variable::nodes_key().as_ref(), parent_nodes.clone());
                    Some(view)
                }
                _ => None,
            };

            walker.set_node_data(
                nid,
                NodeData {
                    name: zen_types::symbol::Symbol::from(node.name.deref()),
                    data: output,
                    nodes_view,
                },
            );

            if matches!(node.kind, DecisionNodeKind::OutputNode { .. }) {
                break;
            }
        }

        let result = walker.ending_variables(&self.graph);
        let trace = tracer.into_traces();

        if self.config.iteration.is_zero() {
            let mut cleaner = VariableCleaner::new();
            cleaner.clean(&result);
            if let Some(t) = &trace {
                t.values().for_each(|v| {
                    cleaner.clean(&v.input);
                    cleaner.clean(&v.output);
                    if let Some(td) = &v.trace_data {
                        cleaner.clean(td);
                    }
                })
            }
        }

        Ok(DecisionGraphResponse {
            performance: format!("{:.1?}", root_start.elapsed()),
            result,
            trace: trace.map(EvaluationTrace::Graph),
        })
    }

    pub(crate) async fn handle_node<NodeData, TraceData, NodeHandlerType>(
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
}

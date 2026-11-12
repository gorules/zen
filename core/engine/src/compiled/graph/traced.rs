use super::schedule::{End, Outcome};
use super::{CompiledGraph, Step};
use crate::compiled::content::Content;
use crate::compiled::host::graph::tracer::{
    NodeTracer, SwitchStatementTrace, SwitchStatementTraceRow,
};
use crate::compiled::host::graph::walker::GraphWalker;
use crate::compiled::host::{
    NodeContextBase, NodeContextConfig, NodeError, NodeHandlerExtensions, NodeResponse, NodeResult,
};
use crate::compiled::pinning::Located;
use crate::decision_graph::cleaner::VariableCleaner;
use crate::decision_graph::graph::{DecisionGraphResponse, EvaluationTrace};
use crate::model::DecisionNodeKind;
use crate::{DecisionGraphTrace, EvaluationError, ZEN_CONFIG};
use ahash::HashMap;
use petgraph::prelude::NodeIndex;
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::lane::Session;
use zen_expression::variable::{ToVariable, Variable};
use zen_types::decision::{SwitchNodeContent, SwitchStatement, SwitchStatementHitPolicy};
use zen_types::symbol::Symbol;

pub(crate) struct Traced<'g> {
    pub(crate) graph: &'g CompiledGraph,
    pub(crate) content: &'g Arc<Content>,
    pub(crate) extensions: &'g NodeHandlerExtensions,
    pub(crate) max_depth: u8,
    pub(crate) iteration: u8,
    pub(crate) parent: Option<Variable>,
}

struct Slot {
    data: Variable,
    view: Option<Variable>,
}

type Traces = HashMap<Arc<str>, DecisionGraphTrace>;

impl Traced<'_> {
    pub(crate) async fn evaluate(
        &self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let root_start = Instant::now();
        if self.iteration >= self.max_depth {
            return Err(Box::new(EvaluationError::DepthLimitExceeded));
        }

        let with_nodes = ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed);
        let mut slots: Vec<Option<Slot>> = (0..self.graph.steps.len()).map(|_| None).collect();
        let mut tracer = NodeTracer::new(true);
        let mut segment = self.graph.root.clone();
        let ending = loop {
            for event in segment.events.iter() {
                let Some(step) = self.graph.step_at(event.node) else {
                    continue;
                };
                let start = Instant::now();
                let input = Self::merged(&slots, &event.parents);
                let nodes = with_nodes.then(|| self.nodes(&slots, &event.visible));
                let result = self.node(step, &context, input.clone(), nodes).await;
                tracer.record_execution(&step.node, input, &result, start.elapsed());
                let output = match result {
                    Ok(response) => response.output,
                    Err(error) => return Err(Self::failed(tracer.into_traces(), error)),
                };
                let view = match (&step.node.kind, &self.parent) {
                    (DecisionNodeKind::InputNode { .. }, Some(parent)) => {
                        let view = output.depth_clone(1);
                        view.dot_insert(Variable::nodes_key().as_ref(), parent.clone());
                        Some(view)
                    }
                    _ => None,
                };
                if let Some(slot) = slots.get_mut(event.node.index()) {
                    *slot = Some(Slot { data: output, view });
                }
            }

            match &segment.end {
                End::Finish(ending) => break ending.clone(),
                End::Switch {
                    node,
                    parents,
                    visible,
                    ..
                } => {
                    let start = Instant::now();
                    let input = Self::merged(&slots, parents);
                    let nodes = with_nodes.then(|| self.nodes(&slots, visible));
                    let outcome = match self.graph.step_at(*node) {
                        Some(step) => self.switch(step, input, nodes, start, &mut tracer),
                        None => Outcome::from(Vec::new()),
                    };
                    segment = self.graph.child(&segment, outcome);
                }
            }
        };

        let result = GraphWalker::merge_ending(
            ending
                .iter()
                .filter_map(|nid| slots.get(nid.index())?.as_ref().map(|slot| &slot.data)),
        );
        let traces = tracer.into_traces();
        if self.iteration == 0 {
            let mut cleaner = VariableCleaner::new();
            cleaner.clean(&result);
            if let Some(traces) = &traces {
                Self::clean(&mut cleaner, traces);
            }
        }

        Ok(DecisionGraphResponse {
            performance: format!("{:.1?}", root_start.elapsed()),
            result,
            trace: traces.map(EvaluationTrace::Graph),
        })
    }

    fn merged(slots: &[Option<Slot>], parents: &[NodeIndex]) -> Variable {
        GraphWalker::merge_values(
            parents
                .iter()
                .filter_map(|nid| slots.get(nid.index())?.as_ref().map(|slot| &slot.data)),
        )
    }

    fn nodes(&self, slots: &[Option<Slot>], visible: &[NodeIndex]) -> Variable {
        Variable::from_object(
            visible
                .iter()
                .filter_map(|nid| {
                    let slot = slots.get(nid.index())?.as_ref()?;
                    let step = self.graph.step_at(*nid)?;
                    let value = slot.view.clone().unwrap_or_else(|| slot.data.clone());
                    Some((Symbol::from(step.node.name.as_ref()), value))
                })
                .collect(),
        )
    }

    fn clean(cleaner: &mut VariableCleaner, traces: &Traces) {
        traces.values().for_each(|trace| {
            cleaner.clean(&trace.input);
            cleaner.clean(&trace.output);
            if let Some(data) = &trace.trace_data {
                cleaner.clean(data);
            }
        });
    }

    fn failed(traces: Option<Traces>, error: NodeError) -> Box<EvaluationError> {
        if let Some(traces) = &traces {
            Self::clean(&mut VariableCleaner::new(), traces);
        }

        Box::new(
            NodeError::traced(
                error.node_id,
                traces.map(|traces| traces.to_variable()),
                error.source,
            )
            .failure(),
        )
    }

    fn switch(
        &self,
        step: &Step,
        input: Variable,
        nodes: Option<Variable>,
        start: Instant,
        tracer: &mut NodeTracer,
    ) -> Outcome {
        let DecisionNodeKind::SwitchNode { content } = &step.node.kind else {
            return Outcome::from(Vec::new());
        };
        let statements = Self::statements(step, content, &input, nodes, &self.extensions.context);
        if let Some(mut record) = tracer.trace_callback() {
            let output = input.depth_clone(1);
            record(DecisionGraphTrace {
                id: step.node.id.clone(),
                name: step.node.name.clone(),
                input,
                output,
                order: 0,
                performance: Some(Arc::from(format!("{:.1?}", start.elapsed()))),
                trace_data: Some(
                    SwitchStatementTrace {
                        statements: statements.clone(),
                    }
                    .to_variable(),
                ),
            });
        }

        statements
            .into_iter()
            .map(|statement| statement.id)
            .collect()
    }

    fn statements(
        step: &Step,
        content: &SwitchNodeContent,
        input: &Variable,
        nodes: Option<Variable>,
        context: &zen_expression::context::EvaluationContext,
    ) -> Vec<SwitchStatementTraceRow> {
        let mut session = Session::with_environment(input.clone())
            .with_programs(Some(step.programs().clone()))
            .with_context(context.clone());
        if let Some(nodes) = nodes {
            session.set_local(Variable::nodes_key(), nodes);
        }

        let node = &step.node.id;
        let mut passes = |statement: &SwitchStatement| {
            if statement.condition.is_empty() {
                return true;
            }
            session.locate(Located::statement(node, &statement.id));
            session
                .run_standard(&statement.condition)
                .is_ok_and(|value| value.as_bool().unwrap_or(false))
        };
        let mut statements = content.statements.iter();
        match content.hit_policy {
            SwitchStatementHitPolicy::First => statements
                .find(|statement| passes(statement))
                .into_iter()
                .cloned()
                .map(SwitchStatementTraceRow::from)
                .collect(),
            SwitchStatementHitPolicy::Collect => statements
                .filter(|statement| passes(statement))
                .cloned()
                .map(SwitchStatementTraceRow::from)
                .collect(),
        }
    }

    async fn node(
        &self,
        step: &Step,
        context: &Variable,
        input: Variable,
        nodes: Option<Variable>,
    ) -> NodeResult {
        let node = &step.node;
        let base = NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            nodes,
            extensions: self.extensions.clone(),
            iteration: self.iteration,
            trace: Some(RefCell::new(Variable::Null)),
            config: NodeContextConfig {
                max_depth: self.max_depth,
                trace: true,
                ..Default::default()
            },
        };

        match &node.kind {
            DecisionNodeKind::InputNode { .. } => {
                let base = NodeContextBase {
                    input: context.clone(),
                    ..base
                };
                self.graph.edge(step, base, self.content).await
            }
            DecisionNodeKind::OutputNode { .. } => self.graph.edge(step, base, self.content).await,
            DecisionNodeKind::SwitchNode { .. } => Ok(NodeResponse {
                output: base.input,
                trace_data: None,
            }),
            _ => CompiledGraph::dispatch(step, base, Some(step.programs())).await,
        }
    }
}

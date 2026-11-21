use super::schedule::{End, Outcome};
use super::{CompiledGraph, Step};
use crate::compiled::content::Content;
use crate::compiled::exact::graph::NodeRecorder;
use crate::compiled::host::graph::walker::GraphWalker;
use crate::compiled::host::{
    NodeContextBase, NodeContextConfig, NodeError, NodeHandlerExtensions, NodeResponse, NodeResult,
};
use crate::compiled::pinning::Located;
use crate::compiled::trace::collect::Slot;
use crate::compiled::trace::record::{Recording, TraceEvent, Tracer};
use crate::model::DecisionNodeKind;
use crate::{EvaluationError, ZEN_CONFIG};
use petgraph::prelude::NodeIndex;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::lane::Session;
use zen_expression::variable::Variable;
use zen_types::decision::{SwitchNodeContent, SwitchStatement, SwitchStatementHitPolicy};
use zen_types::symbol::Symbol;

pub(crate) struct Traced<'g> {
    pub(crate) graph: &'g CompiledGraph,
    pub(crate) content: &'g Arc<Content>,
    pub(crate) extensions: &'g NodeHandlerExtensions,
    pub(crate) iteration: u8,
    pub(crate) parent: Option<Variable>,
}

struct Held {
    data: Variable,
    view: Option<Variable>,
}

pub(crate) struct Run {
    pub(crate) result: Result<Variable, Box<EvaluationError>>,
    pub(crate) recording: Recording,
}

impl Traced<'_> {
    pub(crate) async fn run(&self, context: Variable) -> Run {
        if self.iteration == 0 && self.parent.is_none() {
            crate::compiled::trace::count_run();
        }
        let mut tracer = Tracer::new(self.graph.template(), self.graph.steps.len());
        if self.iteration >= self.extensions.limits.max_depth {
            return Run {
                result: Err(Box::new(EvaluationError::DepthLimitExceeded)),
                recording: tracer.finish(),
            };
        }

        let with_nodes = ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed);
        let mut slots: Vec<Option<Held>> = (0..self.graph.steps.len()).map(|_| None).collect();
        let mut segment = self.graph.root.clone();
        let ending = loop {
            for event in segment.events.iter() {
                let Some(step) = self.graph.step_at(event.node) else {
                    continue;
                };
                let start = Instant::now();
                let input = Self::merged(&slots, &event.parents);
                let nodes = with_nodes.then(|| self.nodes(&slots, &event.visible));
                let recorder = NodeRecorder::default();
                let result = self
                    .node(step, &context, input.clone(), nodes, &recorder)
                    .await;
                let ns = start.elapsed().as_nanos() as u64;
                let switch = matches!(step.node.kind, DecisionNodeKind::SwitchNode { .. });
                if !switch {
                    let (output, detail) = match &result {
                        Ok(response) => (
                            match step.node.kind {
                                DecisionNodeKind::OutputNode { .. } => Variable::Null,
                                _ => response.output.clone(),
                            },
                            recorder.finish(response.trace_data.as_ref()),
                        ),
                        Err(error) => (Variable::Null, recorder.finish(error.trace.as_ref())),
                    };
                    tracer.record(TraceEvent::Node {
                        node: event.node.index() as Slot,
                        parents: Self::parents(&slots, &event.parents),
                        input: match step.node.kind {
                            DecisionNodeKind::InputNode { .. } => Variable::Null,
                            _ => input,
                        },
                        output,
                        ns,
                        detail,
                    });
                }
                let output = match result {
                    Ok(response) => response.output,
                    Err(error) => {
                        return Run {
                            result: Err(Box::new(
                                NodeError::new(error.node_id, error.source).failure(),
                            )),
                            recording: tracer.finish(),
                        }
                    }
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
                    *slot = Some(Held { data: output, view });
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
                        Some(step) => {
                            let (outcome, taken) = self.switch(step, &input, nodes);
                            tracer.record(TraceEvent::Switch {
                                node: node.index() as Slot,
                                parents: Self::parents(&slots, parents),
                                input,
                                ns: start.elapsed().as_nanos() as u64,
                                branches: taken,
                            });
                            outcome
                        }
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
        Run {
            result: Ok(result),
            recording: tracer.finish(),
        }
    }

    fn parents(slots: &[Option<Held>], parents: &[NodeIndex]) -> SmallVec<[Slot; 4]> {
        parents
            .iter()
            .filter(|nid| matches!(slots.get(nid.index()), Some(Some(_))))
            .map(|nid| nid.index() as Slot)
            .collect()
    }

    fn merged(slots: &[Option<Held>], parents: &[NodeIndex]) -> Variable {
        GraphWalker::merge_values(
            parents
                .iter()
                .filter_map(|nid| slots.get(nid.index())?.as_ref().map(|slot| &slot.data)),
        )
    }

    fn nodes(&self, slots: &[Option<Held>], visible: &[NodeIndex]) -> Variable {
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

    fn switch(
        &self,
        step: &Step,
        input: &Variable,
        nodes: Option<Variable>,
    ) -> (Outcome, SmallVec<[u32; 4]>) {
        let DecisionNodeKind::SwitchNode { content } = &step.node.kind else {
            return (Outcome::from(Vec::new()), SmallVec::new());
        };
        let taken = Self::statements(step, content, input, nodes, &self.extensions.context);
        let outcome = taken
            .iter()
            .filter_map(|&at| content.statements.get(at as usize))
            .map(|statement| statement.id.clone())
            .collect();
        (outcome, taken)
    }

    fn statements(
        step: &Step,
        content: &SwitchNodeContent,
        input: &Variable,
        nodes: Option<Variable>,
        context: &zen_expression::context::EvaluationContext,
    ) -> SmallVec<[u32; 4]> {
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
        let mut statements = content.statements.iter().enumerate();
        match content.hit_policy {
            SwitchStatementHitPolicy::First => statements
                .find(|(_, statement)| passes(statement))
                .map(|(at, _)| at as u32)
                .into_iter()
                .collect(),
            SwitchStatementHitPolicy::Collect => statements
                .filter(|(_, statement)| passes(statement))
                .map(|(at, _)| at as u32)
                .collect(),
        }
    }

    async fn node(
        &self,
        step: &Step,
        context: &Variable,
        input: Variable,
        nodes: Option<Variable>,
        recorder: &NodeRecorder,
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
            _ => CompiledGraph::dispatch(step, base, Some(step.programs()), Some(recorder)).await,
        }
    }
}

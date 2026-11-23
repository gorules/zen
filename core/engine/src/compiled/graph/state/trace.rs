use crate::compiled::exact::graph::NodeRecorder;
use crate::compiled::graph::data::Data;
use crate::compiled::graph::schedule::{Execute, Outcome};
use crate::compiled::graph::{Body, CompiledGraph, Kind, State, Step, Transform};
use crate::compiled::host::{
    NodeContextBase, NodeContextConfig, NodeError, NodeHandlerExtensions, Runtime,
};
use crate::compiled::trace::batch::{BatchEvent, Encoding, Facts, Value};
use crate::compiled::trace::collect::{Fact, Slot};
use crate::compiled::trace::record::{LoopBody, NodeShape, Probe};
use crate::model::DecisionNodeKind;
use petgraph::prelude::NodeIndex;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;
use zen_expression::context::EvaluationContext;
use zen_expression::Variable;

impl CompiledGraph {
    fn slots(parents: &[NodeIndex]) -> Box<[Slot]> {
        parents.iter().map(|p| p.index() as Slot).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::compiled::graph) fn trace_event<'a>(
        state: &mut State<'a>,
        event: &Execute,
        rows: &Rc<[usize]>,
        input: bool,
        output: Option<Value<'a>>,
        start: Instant,
        facts: Facts<'a>,
        encoding: Encoding<'a>,
    ) {
        if let Some(traces) = state.traces.as_mut() {
            traces.record_batch(BatchEvent {
                slot: event.node.index() as Slot,
                parents: Self::slots(&event.parents),
                rows: rows.clone(),
                input,
                output,
                ns: start.elapsed().as_nanos() as u64,
                facts,
                encoding,
            });
        }
    }

    pub(in crate::compiled::graph) fn encoding<'a>(
        &self,
        step: &Step,
        event: &Execute,
        data: &Rc<Data<'a>>,
    ) -> Encoding<'a> {
        match &step.kind {
            Kind::Input => Encoding::Request,
            Kind::Output => Encoding::Null,
            Kind::Switch { .. } => Encoding::Passed,
            Kind::Host(_) => Encoding::Generic,
            Kind::Node { transform, .. } => {
                match (
                    self.template()
                        .touched(event.node.index() as Slot)
                        .is_some(),
                    transform.pass_through,
                ) {
                    (true, _) => Encoding::Touched(Some(data.clone())),
                    (false, false) => Encoding::Record,
                    (false, true) => Encoding::Generic,
                }
            }
        }
    }

    pub(in crate::compiled::graph) fn trace_switch(
        &self,
        node: NodeIndex,
        parents: &[NodeIndex],
        rows: &Rc<[usize]>,
        groups: &[(Outcome, Vec<usize>)],
        start: Instant,
        state: &mut State<'_>,
    ) {
        let slot = node.index() as Slot;
        let mut taken: Vec<SmallVec<[u32; 4]>> = vec![SmallVec::new(); rows.len()];
        if let Some(traces) = state.traces.as_ref() {
            for (outcome, members) in groups {
                let positions: SmallVec<[u32; 4]> = outcome
                    .iter()
                    .filter_map(|id| traces.template.statement(slot, id))
                    .collect();
                for row in members {
                    if let Ok(at) = rows.binary_search(row) {
                        taken[at] = positions.clone();
                    }
                }
            }
        }
        let ns = start.elapsed().as_nanos() as u64;
        if let Some(traces) = state.traces.as_mut() {
            traces.record_batch(BatchEvent {
                slot,
                parents: Self::slots(parents),
                rows: rows.clone(),
                input: true,
                output: None,
                ns,
                facts: Facts::Branches(taken),
                encoding: Encoding::Passed,
            });
        }
    }

    pub(in crate::compiled::graph) fn keyed_results(step: &Step) -> bool {
        let (Kind::Node { body, .. }, DecisionNodeKind::ExpressionNode { content }) =
            (&step.kind, &step.node.kind)
        else {
            return false;
        };
        let Body::Expression {
            keys, overlapping, ..
        } = body
        else {
            return false;
        };
        let original: Vec<&std::sync::Arc<str>> = content
            .expressions
            .iter()
            .filter(|e| !e.key.is_empty() && !e.value.is_empty())
            .map(|e| &e.key)
            .collect();
        !overlapping
            && original.len() == keys.len()
            && original.iter().zip(keys.iter()).all(|(a, b)| a == &b)
    }

    pub(in crate::compiled::graph) fn derived_refs(step: &Step) -> bool {
        let DecisionNodeKind::DecisionTableNode { content } = &step.node.kind else {
            return false;
        };
        let scoped = content
            .transform_attributes
            .input_field
            .as_deref()
            .is_none_or(crate::compiled::trace::plain_path);
        scoped
            && content
                .rules
                .iter()
                .all(|rule| !rule.contains_key("_description"))
            && content.inputs.iter().all(|input| match &input.field {
                None => true,
                Some(field) if field.is_empty() => true,
                Some(field) => {
                    crate::compiled::trace::plain_path(field)
                        && zen_expression::lane::LaneProgram::standard(field)
                            .is_ok_and(|program| program.path() == Some(field.as_ref()))
                }
            })
    }

    pub(in crate::compiled::graph) fn kernel_facts<'x>(
        &self,
        step: &Step,
        probe: Option<Rc<Probe>>,
        state: &State<'_>,
        event: &Execute,
        rows: &Rc<[usize]>,
        data: &Data<'_>,
    ) -> Facts<'x> {
        let slot = event.node.index() as Slot;
        let shape = Self::node_shape(step, Some(data));
        let recorded = probe
            .ok_or("evaluated outside the batch kernels")
            .and_then(|probe| {
                probe
                    .finish()
                    .facts(self.template(), slot, shape, rows.len())
            });
        match recorded {
            Ok(facts) => facts,
            Err(reason) => {
                crate::compiled::trace::reevaluated(reason, rows.len());
                Facts::Rows(self.row_facts(step, state, event, rows, data))
            }
        }
    }

    pub(in crate::compiled::graph) fn node_shape(
        step: &Step,
        data: Option<&Data<'_>>,
    ) -> NodeShape {
        let Kind::Node { body, transform } = &step.kind else {
            return NodeShape::Hosted;
        };
        if transform.looped {
            let spans = data.and_then(|data| Self::loop_spans(transform, data));
            let inner = match body {
                Body::Expression { .. } if Self::keyed_results(step) => LoopBody::Results,
                Body::Table(table) if table.first_hit() && Self::derived_refs(step) => {
                    LoopBody::FirstDerived
                }
                _ => LoopBody::Other,
            };
            return NodeShape::Loop { spans, inner };
        }
        match body {
            Body::Expression { .. } => NodeShape::Expression {
                keyed: Self::keyed_results(step),
            },
            Body::Table(table) => NodeShape::Table {
                first: table.first_hit(),
                derived: Self::derived_refs(step),
            },
        }
    }

    pub(in crate::compiled::graph) fn wants_fields(step: &Step) -> bool {
        matches!(
            &step.kind,
            Kind::Node { body: Body::Table(table), transform }
                if !transform.looped && !(table.first_hit() && Self::derived_refs(step))
        )
    }

    fn loop_spans(transform: &Transform, data: &Data<'_>) -> Option<Rc<[(u32, u32)]>> {
        let source = transform
            .input_field
            .as_ref()
            .and_then(|(program, _)| program.path())?;
        let mut spans: Vec<(u32, u32)> = Vec::with_capacity(data.len());
        let mut total = 0u32;
        if let Some(counts) = data.counts(source) {
            for count in counts {
                let count = count?;
                spans.push((total, count));
                total += count;
            }
            return Some(spans.into());
        }
        for list in data.subtree(source).iter() {
            let count = list.as_array()?.borrow().len() as u32;
            spans.push((total, count));
            total += count;
        }
        Some(spans.into())
    }

    pub(in crate::compiled::graph) fn merged_rows(
        state: &State<'_>,
        parents: &[NodeIndex],
        rows: &Rc<[usize]>,
    ) -> Vec<Variable> {
        let mut held: Vec<&[Rc<Data<'_>>]> = parents
            .iter()
            .filter_map(|p| state.nodes.get(p.index()))
            .filter(|pieces| !pieces.is_empty())
            .map(Vec::as_slice)
            .collect();
        held.dedup_by(|a, b| std::ptr::eq(*a, *b));
        let columns: Vec<crate::compiled::graph::data::Col> = held
            .into_iter()
            .map(|pieces| Data::gather(pieces, rows).materialized())
            .collect();
        (0..rows.len())
            .map(|row| {
                crate::compiled::host::graph::walker::GraphWalker::merge_values(
                    columns.iter().map(|c| &c[row]),
                )
            })
            .collect()
    }

    fn row_facts(
        &self,
        step: &Step,
        state: &State<'_>,
        event: &Execute,
        rows: &Rc<[usize]>,
        _data: &Data<'_>,
    ) -> Vec<Fact> {
        let inputs = Self::merged_rows(state, &event.parents, rows);
        let nodes: Vec<Variable> = match step.nodes && state.with_nodes {
            true => self.nodes_of(state, &event.visible, rows),
            false => Vec::new(),
        };
        Self::facts_of(step, &inputs, &nodes, &state.context.picked(rows.clone()))
    }

    pub(in crate::compiled::graph) fn facts_of(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Variable],
        context: &EvaluationContext,
    ) -> Vec<Fact> {
        Self::evaluated(step, inputs, nodes, context).0
    }

    pub(in crate::compiled::graph) fn evaluated(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Variable],
        context: &EvaluationContext,
    ) -> (Vec<Fact>, Vec<Variable>) {
        let mut outputs = Vec::with_capacity(inputs.len());
        let facts = inputs
            .iter()
            .enumerate()
            .map(|(position, input)| {
                let recorder = NodeRecorder::default();
                outputs.push(
                    Self::walked(
                        step,
                        input.clone(),
                        nodes.get(position).cloned(),
                        true,
                        &context.at(position),
                        Some(&recorder),
                    )
                    .unwrap_or(Variable::Null),
                );
                recorder.finish(None)
            })
            .collect();
        (facts, outputs)
    }

    pub(in crate::compiled::graph) async fn hosted_traced(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
    ) -> (Vec<Result<Variable, NodeError>>, Vec<Fact>) {
        let shared: Option<Runtime> = match &step.node.kind {
            DecisionNodeKind::FunctionNode {
                content: zen_types::decision::FunctionNodeContent::Version2(content),
            } => {
                let source = extensions
                    .stripped_functions
                    .as_ref()
                    .and_then(|stripped| stripped.get(content.source.as_ref()).cloned())
                    .unwrap_or_else(|| {
                        crate::compiled::host::function::v2::strip::TypeStripper::strip(
                            content.source.as_ref(),
                        )
                    });
                match crate::compiled::host::function::purity::Purity::module(&source) {
                    true => {
                        let runtime = Runtime::default();
                        let shared = NodeHandlerExtensions {
                            function_runtime: runtime.clone(),
                            ..extensions.clone()
                        };
                        if let Ok(function) = shared.function_runtime().await {
                            let _ = function.reuse_module(&step.node.id, &source).await;
                        }
                        Some(runtime)
                    }
                    false => None,
                }
            }
            _ => None,
        };
        let mut results = Vec::with_capacity(inputs.len());
        let mut facts = Vec::with_capacity(inputs.len());
        for (i, input) in inputs.iter().enumerate() {
            let node = &step.node;
            let recorder = NodeRecorder::default();
            let base = NodeContextBase {
                id: node.id.clone(),
                name: node.name.clone(),
                input: input.clone(),
                nodes: nodes.get(i).cloned().flatten(),
                extensions: NodeHandlerExtensions {
                    function_runtime: shared.clone().unwrap_or_default(),
                    context: extensions.context.at(i),
                    ..extensions.clone()
                },
                trace: Some(RefCell::new(Variable::Null)),
                config: NodeContextConfig {
                    trace: true,
                    ..Default::default()
                },
            };
            let result = Box::pin(Self::dispatch(
                step,
                base,
                Some(step.programs()),
                Some(&recorder),
            ))
            .await;
            facts.push(recorder.finish(match &result {
                Ok(response) => response.trace_data.as_ref(),
                Err(error) => error.trace.as_ref(),
            }));
            results.push(result.map(|response| response.output));
        }
        (results, facts)
    }
}

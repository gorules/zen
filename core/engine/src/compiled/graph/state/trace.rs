use crate::compiled::exact::graph::{Row, Sink};
use crate::compiled::graph::data::Data;
use crate::compiled::graph::schedule::{Execute, Outcome};
use crate::compiled::graph::tap::{Field, Tap};
use crate::compiled::graph::traces::{Encoding, Event, Facts, Value};
use crate::compiled::graph::{Body, CompiledGraph, Kind, State, Step};
use crate::compiled::host::{
    NodeContextBase, NodeContextConfig, NodeError, NodeHandlerExtensions, Runtime,
};
use crate::compiled::table::Table;
use crate::compiled::trace::collect::{Fact, Hit, Slot, Template};
use crate::model::DecisionNodeKind;
use petgraph::prelude::NodeIndex;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;
use zen_expression::clock::EvalContext;
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
        facts: Facts,
        encoding: Encoding<'a>,
    ) {
        if let Some(traces) = state.traces.as_mut() {
            traces.push(Event {
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
            traces.push(Event {
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

    fn columnar_results(step: &Step) -> bool {
        matches!(&step.kind, Kind::Node { transform, .. } if !transform.looped)
            && Self::keyed_results(step)
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

    pub(in crate::compiled::graph) fn tapped(
        &self,
        step: &Step,
        tap: Option<Tap>,
        state: &State<'_>,
        event: &Execute,
        rows: &Rc<[usize]>,
        data: &Data<'_>,
    ) -> Facts {
        let slot = event.node.index() as Slot;
        let reason = match Self::tap_facts(self.template(), step, tap, slot, rows.len(), Some(data))
        {
            Ok(facts) => return facts,
            Err(reason) => reason,
        };
        crate::compiled::trace::reevaluated(reason, rows.len());
        Facts::Rows(self.row_facts(step, state, event, rows, data))
    }

    pub(in crate::compiled::graph) fn tap_facts(
        template: &Template,
        step: &Step,
        tap: Option<Tap>,
        slot: Slot,
        count: usize,
        data: Option<&Data<'_>>,
    ) -> Result<Facts, &'static str> {
        let Kind::Node { body, transform } = &step.kind else {
            return Err("hosted node");
        };
        let tap = tap
            .filter(|tap| tap.calls == 1)
            .ok_or("evaluated outside the batch kernels")?;
        if transform.looped {
            return data
                .and_then(|data| Self::looped_facts(step, tap, data))
                .ok_or("loop");
        }
        match body {
            Body::Expression { .. } if Self::columnar_results(step) => {
                if let Some(results) = tap
                    .results
                    .filter(|(_, leaves)| leaves.iter().all(|leaf| leaf.len() == count))
                {
                    return Ok(Facts::Results(results));
                }
                tap.objects
                    .filter(|(_, values)| values.len() == count)
                    .map(|(keys, values)| Facts::Objects(keys, values))
                    .ok_or("expression results not captured")
            }
            Body::Expression { .. } => Err("expression without one result per key"),
            Body::Table(table) => Self::table_facts(template, step, table, tap, count, slot),
        }
    }

    pub(in crate::compiled::graph) fn wants_fields(step: &Step) -> bool {
        matches!(
            &step.kind,
            Kind::Node { body: Body::Table(table), transform }
                if !transform.looped && !(table.first_hit() && Self::derived_refs(step))
        )
    }

    pub(in crate::compiled::graph) fn table_facts(
        template: &Template,
        step: &Step,
        table: &Table,
        tap: Tap,
        count: usize,
        slot: Slot,
    ) -> Result<Facts, &'static str> {
        let derived = Self::derived_refs(step);
        if table.first_hit() {
            let first = tap
                .first
                .filter(|first| first.len() == count)
                .ok_or("table rules not captured")?;
            if derived {
                return Ok(Facts::First(first));
            }
            let fields = tap.fields.as_deref().ok_or("table fields not captured")?;
            let facts = first
                .iter()
                .enumerate()
                .map(|(position, rule)| match rule {
                    None => Some(Fact::None),
                    Some(rule) => {
                        Self::captured_hit(template, slot, *rule, fields, position).map(Fact::Hit)
                    }
                })
                .collect::<Option<Vec<Fact>>>()
                .ok_or("table field references")?;
            return Ok(Facts::Rows(facts));
        }
        let hits = tap
            .hits
            .filter(|hits| hits.len() == count)
            .ok_or("collected rules not captured")?;
        let fields = tap.fields.as_deref().ok_or("table fields not captured")?;
        let facts = hits
            .iter()
            .enumerate()
            .map(|(position, rules)| {
                rules
                    .iter()
                    .map(|rule| Self::captured_hit(template, slot, *rule, fields, position))
                    .collect::<Option<Vec<Hit>>>()
                    .map(Fact::Hits)
            })
            .collect::<Option<Vec<Fact>>>()
            .ok_or("table field references")?;
        Ok(Facts::Rows(facts))
    }

    fn captured_hit(
        template: &Template,
        slot: Slot,
        rule: u32,
        fields: &[Field],
        position: usize,
    ) -> Option<Hit> {
        let refs = template.refs_with(slot, rule, |name| {
            let field = fields.iter().find(|f| &*f.name == name)?;
            Some(match field.failed.get(position).copied().unwrap_or(true) {
                true => None,
                false => Some(field.values.get(position)?.clone()),
            })
        })?;
        Some(Hit {
            rule,
            refs: Some(refs),
        })
    }

    pub(in crate::compiled::graph) fn looped_facts(
        step: &Step,
        tap: Tap,
        data: &Data<'_>,
    ) -> Option<Facts> {
        let Kind::Node { body, transform } = &step.kind else {
            return None;
        };
        let source = transform
            .input_field
            .as_ref()
            .and_then(|(program, _)| program.path())?;
        let mut spans: Vec<(u32, u32)> = Vec::with_capacity(data.len());
        let mut total = 0u32;
        for list in data.subtree(source).iter() {
            let count = list.as_array()?.borrow().len() as u32;
            spans.push((total, count));
            total += count;
        }
        let total = total as usize;
        let inner = match body {
            Body::Expression { .. } if Self::keyed_results(step) => {
                let results = tap
                    .results
                    .filter(|(_, leaves)| leaves.iter().all(|leaf| leaf.len() == total))?;
                Facts::Results(results)
            }
            Body::Table(table) if table.first_hit() && Self::derived_refs(step) => {
                Facts::First(tap.first.filter(|first| first.len() == total)?)
            }
            _ => return None,
        };
        Some(Facts::Loop(spans.into(), Box::new(inner)))
    }

    pub(in crate::compiled::graph) fn merged_rows(
        state: &State<'_>,
        parents: &[NodeIndex],
        rows: &Rc<[usize]>,
    ) -> Vec<Variable> {
        let columns: Vec<crate::compiled::graph::data::Col> = parents
            .iter()
            .filter_map(|p| {
                let pieces = state.nodes.get(p.index())?;
                (!pieces.is_empty()).then(|| Data::gather(pieces, rows).materialized())
            })
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
        Self::facts_of(step, &inputs, &nodes, &state.eval.picked(rows.clone()))
    }

    pub(in crate::compiled::graph) fn facts_of(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Variable],
        eval: &EvalContext,
    ) -> Vec<Fact> {
        Self::evaluated(step, inputs, nodes, eval).0
    }

    pub(in crate::compiled::graph) fn evaluated(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Variable],
        eval: &EvalContext,
    ) -> (Vec<Fact>, Vec<Variable>) {
        let programs = Some(step.programs());
        let mut outputs = Vec::with_capacity(inputs.len());
        let facts = inputs
            .iter()
            .enumerate()
            .map(|(position, input)| {
                let sink = Sink::new(Fact::None);
                let row = Row {
                    id: &step.node.id,
                    input: input.clone(),
                    nodes: nodes.get(position).cloned(),
                    programs,
                    sink: Some(&sink),
                    eval: eval.at(position),
                };
                outputs.push(
                    row.now(&step.node, None, true)
                        .map(|response| response.output)
                        .unwrap_or(Variable::Null),
                );
                sink.into_inner()
            })
            .collect();
        (facts, outputs)
    }

    pub(in crate::compiled::graph) async fn hosted_traced(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
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
                crate::compiled::host::function::purity::Purity::module(&source)
                    .then(Runtime::default)
            }
            _ => None,
        };
        let mut results = Vec::with_capacity(inputs.len());
        let mut facts = Vec::with_capacity(inputs.len());
        for (i, input) in inputs.iter().enumerate() {
            let node = &step.node;
            let sink = Sink::new(Fact::None);
            let base = NodeContextBase {
                id: node.id.clone(),
                name: node.name.clone(),
                input: input.clone(),
                nodes: nodes.get(i).cloned().flatten(),
                extensions: NodeHandlerExtensions {
                    function_runtime: shared.clone().unwrap_or_default(),
                    eval: extensions.eval.at(i),
                    ..extensions.clone()
                },
                iteration,
                trace: Some(RefCell::new(Variable::Null)),
                config: NodeContextConfig {
                    max_depth,
                    trace: true,
                    ..Default::default()
                },
            };
            let result = Box::pin(Self::dispatch(
                step,
                base,
                Some(step.programs()),
                Some(&sink),
            ))
            .await;
            let fact = sink.into_inner();
            let fact = match (&fact, &result) {
                (Fact::None, Ok(response)) => match &response.trace_data {
                    Some(td) if !matches!(td, Variable::Null) => Fact::Data(td.clone()),
                    _ => fact,
                },
                (Fact::None, Err(error)) => match &error.trace {
                    Some(td) if !matches!(td, Variable::Null) => Fact::Data(td.clone()),
                    _ => fact,
                },
                _ => fact,
            };
            facts.push(fact);
            results.push(result.map(|response| response.output));
        }
        (results, facts)
    }
}

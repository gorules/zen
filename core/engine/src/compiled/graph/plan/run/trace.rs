use super::store::Store;
use super::Run;
use crate::compiled::graph::data::Data;
use crate::compiled::graph::plan::{Op, SegPlan};
use crate::compiled::graph::schedule::End;
use crate::compiled::graph::tap::Tap;
use crate::compiled::graph::traces::{Encoding, Event, Facts, Value};
use crate::compiled::graph::{Body, CompiledGraph, Kind};
use crate::compiled::host::graph::walker::GraphWalker;
use crate::compiled::leaf::Leaf;
use crate::compiled::trace::collect::{Fact, Slot};
use petgraph::prelude::NodeIndex;
use smallvec::SmallVec;
use std::rc::Rc;
use std::time::Instant;
use zen_expression::Variable;

fn trace_hosted(
    trace: &mut Option<Box<PlanTrace>>,
    node: NodeIndex,
    rows: usize,
) -> Option<Hosted> {
    let trace = trace.as_mut()?;
    let at = trace.hosted.iter().position(|h| h.node == node)?;
    let hosted = trace.hosted.swap_remove(at);
    (hosted.values.len() == rows).then_some(hosted)
}

type Column = (Rc<[usize]>, Rc<[Variable]>);

struct Hosted {
    node: NodeIndex,
    values: Rc<[Variable]>,
    tap: Option<Tap>,
    walked: Vec<Fact>,
}
pub(super) type Groups = [(Vec<u64>, Option<Rc<[usize]>>)];

#[derive(Default)]
pub(crate) struct PlanTrace {
    pub(crate) events: Vec<Event<'static>>,
    taps: Vec<(NodeIndex, Option<Tap>)>,
    hosted: Vec<Hosted>,
    times: Vec<(NodeIndex, u64)>,
    values: Vec<Option<Column>>,
    pub(crate) refused: Option<&'static str>,
}

impl PlanTrace {
    pub(crate) fn new(slots: usize) -> Self {
        Self {
            values: vec![None; slots],
            ..Self::default()
        }
    }

    fn refuse(&mut self, reason: &'static str) {
        self.refused.get_or_insert(reason);
    }

    fn value(&self, slot: Slot, row: usize) -> Option<Variable> {
        let (rows, column) = self.values.get(slot as usize)?.as_ref()?;
        let at = rows.binary_search(&row).ok()?;
        column.get(at).cloned()
    }

    fn merged(&self, parents: &[NodeIndex], rows: &[usize]) -> Vec<Variable> {
        rows.iter()
            .map(|&row| {
                let values: SmallVec<[Variable; 4]> = parents
                    .iter()
                    .filter_map(|p| self.value(p.index() as Slot, row))
                    .collect();
                GraphWalker::merge_values(values.iter())
            })
            .collect()
    }
}

impl PlanTrace {
    fn nodes(&self, graph: &CompiledGraph, visible: &[NodeIndex], rows: &[usize]) -> Vec<Variable> {
        rows.iter()
            .map(|&row| {
                let object = Variable::empty_object();
                if let Variable::Object(map) = &object {
                    let mut map = map.borrow_mut();
                    for nid in visible {
                        let (Some(step), Some(value)) =
                            (graph.step_at(*nid), self.value(nid.index() as Slot, row))
                        else {
                            continue;
                        };
                        map.insert(step.node.name.as_ref().into(), value);
                    }
                }
                object
            })
            .collect()
    }
}

impl<'a> Run<'_, 'a> {
    pub(super) fn traced_op(
        &mut self,
        seg: &SegPlan,
        op: &Op,
        positions: Option<&Rc<[usize]>>,
    ) -> bool {
        match op {
            Op::Typed { units, .. } | Op::Deferred { units, .. } => {
                let mut seen = self.store.errors.generation();
                let mut live = positions.cloned();
                for unit in units {
                    if self.store.errors.generation() != seen {
                        live = self.alive(live, &mut seen);
                    }
                    self.traced_unit(seg, unit, live.as_ref());
                }
                true
            }
            Op::Rows(rows)
                if self
                    .graph
                    .step_at(rows.node)
                    .is_none_or(|step| matches!(step.kind, Kind::Host(_))) =>
            {
                self.refuse("hosted node");
                false
            }
            Op::Lane(_) | Op::Table(_) | Op::Loop { .. } => {
                self.traced_unit(seg, op, positions);
                true
            }
            Op::Child(_) => {
                self.refuse("sub-decision");
                false
            }
            _ => false,
        }
    }

    fn refuse(&mut self, reason: &'static str) {
        if let Some(trace) = self.trace.as_mut() {
            trace.refuse(reason);
        }
    }

    pub(super) fn hosted_rows(
        &mut self,
        node: NodeIndex,
        output: &Data<'_>,
        tap: Option<Tap>,
        walked: Vec<Fact>,
    ) {
        if let Some(trace) = self.trace.as_mut() {
            trace.hosted.push(Hosted {
                node,
                values: output
                    .materialized()
                    .iter()
                    .map(Variable::deep_clone)
                    .collect(),
                tap,
                walked,
            });
        }
    }

    pub(super) fn timed_rows(&mut self, node: NodeIndex, start: Instant) {
        if let Some(trace) = self.trace.as_mut() {
            trace.times.push((node, start.elapsed().as_nanos() as u64));
        }
    }

    fn traced_unit(&mut self, seg: &SegPlan, op: &Op, positions: Option<&Rc<[usize]>>) {
        let node = match op {
            Op::Lane(lane) => lane.node,
            Op::Table(table) => table.node,
            Op::Loop { node, .. } => *node,
            _ => {
                self.unit(seg, op, positions);
                return;
            }
        };
        let start = Instant::now();
        Tap::arm(
            self.graph
                .step_at(node)
                .is_some_and(CompiledGraph::wants_fields),
        );
        match op {
            Op::Lane(lane) => self.lane(seg, lane, positions, true),
            Op::Table(table) => self.table(seg, table, positions),
            Op::Loop { node, list, out } => self.looped(seg, *node, *list, *out, positions),
            _ => {}
        }
        let tap = Tap::take();
        let ns = start.elapsed().as_nanos() as u64;
        if let Some(trace) = self.trace.as_mut() {
            trace.times.push((node, ns));
            trace.taps.push((node, tap));
        }
    }

    fn captured(
        &mut self,
        seg: &SegPlan,
        view: &crate::compiled::graph::plan::view::Node,
        rows: &[usize],
    ) -> Rc<[Variable]> {
        let shape = self.store.shape(seg, view);
        match rows.len() == self.store.rows {
            true => Store::objects(&shape, self.store.rows).into(),
            false => rows.iter().map(|&row| Store::object(&shape, row)).collect(),
        }
    }

    pub(super) fn capture(&mut self, seg: &SegPlan, positions: Option<&Rc<[usize]>>) {
        if self.trace.as_ref().is_none_or(|t| t.refused.is_some()) {
            return;
        }
        let rows: Rc<[usize]> = match positions {
            Some(positions) => positions.clone(),
            None => (0..self.store.rows).collect(),
        };
        let graph = self.graph;
        for event in seg.segment.events.iter() {
            let Some(step) = graph.step_at(event.node) else {
                continue;
            };
            let slot = event.node.index() as Slot;
            let Some(trace) = self.trace.as_mut() else {
                return;
            };
            let ns = trace
                .times
                .iter()
                .filter(|(n, _)| *n == event.node)
                .map(|(_, ns)| *ns)
                .sum();
            let tap = trace
                .taps
                .iter()
                .position(|(n, _)| *n == event.node)
                .and_then(|at| trace.taps.swap_remove(at).1);
            let parents: Box<[Slot]> = event.parents.iter().map(|p| p.index() as Slot).collect();
            let (input, values, output, facts, encoding) = match &step.kind {
                Kind::Input => {
                    let Some(view) = seg
                        .views
                        .iter()
                        .rev()
                        .find(|(n, _)| *n == event.node)
                        .map(|(_, v)| v.clone())
                    else {
                        self.refuse("node without a view");
                        return;
                    };
                    let values = self.captured(seg, &view, &rows);
                    let output = Rc::new(Data::plain(rows.clone(), values.clone()));
                    (
                        false,
                        values,
                        Value::Data(output),
                        Facts::None,
                        Encoding::Request,
                    )
                }
                Kind::Output => {
                    let Some(trace) = self.trace.as_ref() else {
                        return;
                    };
                    let values: Rc<[Variable]> = trace.merged(&event.parents, &rows).into();
                    (true, values, Value::Null, Facts::None, Encoding::Null)
                }
                Kind::Node { transform, .. }
                    if !seg.results.iter().any(|(n, _)| *n == event.node) =>
                {
                    let nodes = match step.nodes
                        && crate::ZEN_CONFIG
                            .nodes_in_context
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        true => match self.trace.as_ref() {
                            Some(trace) => trace.nodes(graph, &event.visible, &rows),
                            None => return,
                        },
                        false => Vec::new(),
                    };
                    let Some(inputs) = self.merged_inputs(event, &rows) else {
                        return;
                    };
                    let hosted = trace_hosted(&mut self.trace, event.node, rows.len());
                    let (facts, values): (Facts, Rc<[Variable]>) = match hosted {
                        Some(hosted) if hosted.walked.len() == rows.len() => {
                            (Facts::Rows(hosted.walked), hosted.values)
                        }
                        Some(hosted) => {
                            let data = Data::plain(rows.clone(), inputs.clone().into());
                            let facts = CompiledGraph::tap_facts(
                                graph.template(),
                                step,
                                hosted.tap,
                                slot,
                                rows.len(),
                                Some(&data),
                            )
                            .unwrap_or_else(|reason| {
                                crate::compiled::trace::reevaluated(reason, rows.len());
                                Facts::Rows(CompiledGraph::facts_of(step, &inputs, &nodes))
                            });
                            (facts, hosted.values)
                        }
                        None => {
                            crate::compiled::trace::reevaluated(
                                "plan hosts the node row by row",
                                rows.len(),
                            );
                            let (facts, outputs) = CompiledGraph::evaluated(step, &inputs, &nodes);
                            (Facts::Rows(facts), outputs.into())
                        }
                    };
                    let touched = graph.template().touched(slot).is_some();
                    let encoding = match (transform.pass_through, touched) {
                        (false, _) => Encoding::Checked(false),
                        (true, true) => Encoding::Touched(None),
                        (true, false) => Encoding::Checked(true),
                    };
                    let output = Rc::new(Data::plain(rows.clone(), values.clone()));
                    (true, values, Value::Data(output), facts, encoding)
                }
                Kind::Node { transform, body } => {
                    let Some(own) = seg
                        .results
                        .iter()
                        .rev()
                        .find(|(n, _)| *n == event.node)
                        .map(|(_, v)| v.clone())
                    else {
                        self.refuse("node without its own view");
                        return;
                    };
                    let own = self.captured(seg, &own, &rows);
                    let values: Rc<[Variable]> = match transform.pass_through {
                        false => own.clone(),
                        true => {
                            let Some(trace) = self.trace.as_ref() else {
                                return;
                            };
                            let before = trace.merged(&event.parents, &rows);
                            before
                                .into_iter()
                                .zip(own.iter())
                                .map(|(mut before, own)| before.merge_clone(own))
                                .collect()
                        }
                    };
                    let facts =
                        match self.facts(step, event, body, transform.looped, tap, &rows, &own) {
                            Some(facts) => facts,
                            None => return,
                        };
                    let touched = graph.template().touched(slot).is_some();
                    let complete = match (&facts, transform.pass_through, transform.looped) {
                        (Facts::Results((keys, _)), false, false) => {
                            let prefix = transform.output_path.as_deref();
                            let paths: Vec<String> = keys
                                .iter()
                                .map(|key| match prefix {
                                    Some(prefix) => format!("{prefix}.{key}"),
                                    None => key.to_string(),
                                })
                                .collect();
                            Some(
                                own.iter()
                                    .map(|o| paths.iter().all(|p| o.dot(p).is_some()))
                                    .collect::<Rc<[bool]>>(),
                            )
                        }
                        _ => None,
                    };
                    let encoding = match (transform.pass_through, touched, complete) {
                        (false, _, Some(complete)) => Encoding::Results(complete),
                        (false, _, None) => Encoding::Checked(false),
                        (true, true, _) => Encoding::Touched(None),
                        (true, false, _) => Encoding::Checked(true),
                    };
                    let output = Rc::new(Data::plain(rows.clone(), values.clone()));
                    (true, values, Value::Data(output), facts, encoding)
                }
                Kind::Switch { .. } | Kind::Host(_) => {
                    self.refuse("hosted node");
                    return;
                }
            };
            let Some(trace) = self.trace.as_mut() else {
                return;
            };
            trace.events.push(Event {
                slot,
                parents,
                rows: rows.clone(),
                input,
                output: Some(output),
                ns,
                facts,
                encoding,
            });
            if let Some(entry) = trace.values.get_mut(slot as usize) {
                *entry = Some((rows.clone(), values));
            }
        }
        if let Some(trace) = self.trace.as_mut() {
            trace.taps.clear();
            trace.hosted.clear();
            trace.times.clear();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn facts(
        &mut self,
        step: &crate::compiled::graph::Step,
        event: &crate::compiled::graph::schedule::Execute,
        body: &Body,
        looped: bool,
        tap: Option<Tap>,
        rows: &Rc<[usize]>,
        own: &Rc<[Variable]>,
    ) -> Option<Facts> {
        let count = rows.len();
        let reason = match (looped, body) {
            (true, _) => match tap.filter(|tap| tap.calls == 1) {
                Some(tap) => {
                    let data = Data::plain(rows.clone(), self.merged_inputs(event, rows)?.into());
                    match CompiledGraph::looped_facts(step, tap, &data) {
                        Some(facts) => return Some(facts),
                        None => "loop",
                    }
                }
                None => "evaluated outside the batch kernels",
            },
            (false, Body::Expression { keys, .. }) if CompiledGraph::keyed_results(step) => {
                let prefix = match &step.kind {
                    Kind::Node { transform, .. } => transform.output_path.clone(),
                    _ => None,
                };
                let leaves = keys
                    .iter()
                    .map(|key| {
                        let path = match &prefix {
                            Some(prefix) => format!("{prefix}.{key}"),
                            None => key.to_string(),
                        };
                        Leaf::Any(
                            own.iter()
                                .map(|o| o.dot(&path).unwrap_or(Variable::Null))
                                .collect(),
                        )
                    })
                    .collect();
                return Some(Facts::Results((keys.clone(), leaves)));
            }
            (false, Body::Expression { .. }) => "expression without one result per key",
            (false, Body::Table(table)) => {
                let slot = event.node.index() as Slot;
                match tap.filter(|tap| tap.calls == 1).map(|tap| {
                    CompiledGraph::table_facts(self.graph.template(), step, table, tap, count, slot)
                }) {
                    Some(Ok(facts)) => return Some(facts),
                    Some(Err(reason)) => reason,
                    None => "evaluated outside the batch kernels",
                }
            }
        };
        if step.nodes {
            self.refuse("node reads $nodes");
            return None;
        }
        crate::compiled::trace::reevaluated(reason, count);
        let inputs = self.merged_inputs(event, rows)?;
        Some(Facts::Rows(CompiledGraph::facts_of(step, &inputs, &[])))
    }

    fn merged_inputs(
        &self,
        event: &crate::compiled::graph::schedule::Execute,
        rows: &[usize],
    ) -> Option<Vec<Variable>> {
        Some(self.trace.as_ref()?.merged(&event.parents, rows))
    }

    pub(super) fn trace_switch(
        &mut self,
        seg: &SegPlan,
        positions: Option<&Rc<[usize]>>,
        ids: &[std::sync::Arc<str>],
        groups: &Groups,
        ns: u64,
    ) {
        if self.trace.as_ref().is_none_or(|t| t.refused.is_some()) {
            return;
        }
        let End::Switch { node, parents, .. } = &seg.segment.end else {
            return;
        };
        let rows: Rc<[usize]> = match positions {
            Some(positions) => positions.clone(),
            None => (0..self.store.rows).collect(),
        };
        let slot = node.index() as Slot;
        let template = self.graph.template().clone();
        let statements: Vec<Option<u32>> =
            ids.iter().map(|id| template.statement(slot, id)).collect();
        let mut taken: Vec<SmallVec<[u32; 4]>> = vec![SmallVec::new(); rows.len()];
        for (key, members) in groups {
            let picked: SmallVec<[u32; 4]> = (0..ids.len())
                .filter(|i| key.get(i / 64).is_some_and(|w| w & (1 << (i % 64)) != 0))
                .filter_map(|i| statements[i])
                .collect();
            match members {
                None => taken.iter_mut().for_each(|t| *t = picked.clone()),
                Some(members) => {
                    for row in members.iter() {
                        if let Ok(at) = rows.binary_search(row) {
                            taken[at] = picked.clone();
                        }
                    }
                }
            }
        }
        let Some(trace) = self.trace.as_mut() else {
            return;
        };
        let merged: Rc<[Variable]> = trace.merged(parents, &rows).into();
        trace.events.push(Event {
            slot,
            parents: parents.iter().map(|p| p.index() as Slot).collect(),
            rows: rows.clone(),
            input: true,
            output: None,
            ns,
            facts: Facts::Branches(taken),
            encoding: Encoding::Passed,
        });
        if let Some(entry) = trace.values.get_mut(slot as usize) {
            *entry = Some((rows, merged));
        }
    }
}

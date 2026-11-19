use super::store::{Columned, Shaped, Store};
use super::Run;
use crate::compiled::graph::data::Data;
use crate::compiled::graph::plan::{Op, SegPlan};
use crate::compiled::graph::schedule::End;
use crate::compiled::graph::{Body, CompiledGraph, Kind};
use crate::compiled::leaf::{Bits, Leaf};
use crate::compiled::trace::batch::{BatchEvent, Cols, Encoding, Facts, Value};
use crate::compiled::trace::collect::{Fact, Slot};
use crate::compiled::trace::record::{KernelFacts, NodeShape, Probe};
use petgraph::prelude::NodeIndex;
use smallvec::SmallVec;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::Variable;

fn trace_hosted(
    trace: &mut Option<Box<PlanTrace<'_>>>,
    node: NodeIndex,
    rows: usize,
) -> Option<Hosted> {
    let trace = trace.as_mut()?;
    let at = trace.hosted.iter().position(|h| h.node == node)?;
    let hosted = trace.hosted.swap_remove(at);
    (hosted.values.len() == rows).then_some(hosted)
}

struct Hosted {
    node: NodeIndex,
    values: Rc<[Variable]>,
    kernel: Option<KernelFacts>,
    walked: Vec<Fact>,
}
pub(super) type Groups = [(Vec<u64>, Option<Rc<[usize]>>)];

enum Own<'o, 'a> {
    Values(&'o Rc<[Variable]>),
    Columns(&'o Columned<'a>),
}

pub(crate) enum Source<'a> {
    Rows {
        requests: Rc<[Variable]>,
        shared: bool,
    },
    Derived {
        data: Rc<Data<'a>>,
        reserved: crate::compiled::trace::batch::Reserved<'a>,
    },
}

pub(crate) struct PlanTrace<'a> {
    pub(crate) events: Vec<BatchEvent<'a>>,
    kernels: Vec<(NodeIndex, Option<KernelFacts>)>,
    probe: Option<Rc<Probe>>,
    hosted: Vec<Hosted>,
    times: Vec<(NodeIndex, u64)>,
    values: Vec<Option<Rc<Data<'a>>>>,
    pub(crate) source: Source<'a>,
    lazy: bool,
    pub(crate) refused: Option<&'static str>,
}

impl<'a> PlanTrace<'a> {
    pub(crate) fn new(slots: usize, source: Source<'a>, lazy: bool) -> Self {
        Self {
            events: Vec::new(),
            kernels: Vec::new(),
            probe: None,
            hosted: Vec::new(),
            times: Vec::new(),
            values: vec![None; slots],
            source,
            lazy,
            refused: None,
        }
    }

    fn refuse(&mut self, reason: &'static str) {
        self.refused.get_or_insert(reason);
    }

    fn piece(&self, slot: Slot, rows: &Rc<[usize]>) -> Option<Rc<Data<'a>>> {
        let piece = self.values.get(slot as usize)?.as_ref()?;
        match Rc::ptr_eq(&piece.rows, rows) || piece.rows == *rows {
            true => Some(piece.clone()),
            false => Some(Rc::new(Data::gather(std::slice::from_ref(piece), rows))),
        }
    }

    fn input(&self, parents: &[NodeIndex], rows: &Rc<[usize]>) -> Rc<Data<'a>> {
        let mut gathered: Vec<Rc<Data<'a>>> = parents
            .iter()
            .filter_map(|parent| self.piece(parent.index() as Slot, rows))
            .collect();
        if let [single] = gathered.as_slice() {
            if single.single() {
                return gathered.swap_remove(0);
            }
        }
        Rc::new(Data::merged(
            rows,
            gathered.iter().map(|piece| piece.copied()).collect(),
        ))
    }

    fn nodes(
        &self,
        graph: &CompiledGraph,
        visible: &[NodeIndex],
        rows: &Rc<[usize]>,
    ) -> Vec<Variable> {
        let named: Vec<(&str, Rc<[Variable]>)> = visible
            .iter()
            .filter_map(|nid| {
                let step = graph.step_at(*nid)?;
                let piece = self.piece(nid.index() as Slot, rows)?;
                Some((step.node.name.as_ref(), piece.materialized()))
            })
            .collect();
        (0..rows.len())
            .map(|at| {
                let object = Variable::empty_object();
                if let Variable::Object(map) = &object {
                    let mut map = map.borrow_mut();
                    for (name, values) in &named {
                        if let Some(value) = values.get(at) {
                            map.insert((*name).into(), value.clone());
                        }
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

    pub(super) fn probe(&self) -> Option<Rc<Probe>> {
        self.trace.as_ref()?.probe.clone()
    }

    pub(super) fn hosted_rows(
        &mut self,
        node: NodeIndex,
        output: &Data<'_>,
        kernel: Option<KernelFacts>,
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
                kernel,
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
        let probe = Probe::new(
            self.graph
                .step_at(node)
                .is_some_and(CompiledGraph::wants_fields),
        );
        if let Some(trace) = self.trace.as_mut() {
            trace.probe = Some(probe.clone());
        }
        match op {
            Op::Lane(lane) => self.lane(seg, lane, positions, true),
            Op::Table(table) => self.table(seg, table, positions),
            Op::Loop { node, list, out } => self.looped(seg, *node, *list, *out, positions),
            _ => {}
        }
        let ns = start.elapsed().as_nanos() as u64;
        if let Some(trace) = self.trace.as_mut() {
            trace.probe = None;
            trace.times.push((node, ns));
            trace.kernels.push((node, Some(probe.finish())));
        }
    }

    fn captured(&self, shape: &Shaped<'a>, rows: &[usize]) -> Rc<[Variable]> {
        match rows.len() == self.store.rows {
            true => Store::objects(shape, self.store.rows).into(),
            false => rows.iter().map(|&row| Store::object(shape, row)).collect(),
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
        let total = self.store.rows;
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
            let kernel = trace
                .kernels
                .iter()
                .position(|(n, _)| *n == event.node)
                .and_then(|at| trace.kernels.swap_remove(at).1);
            let parents: Box<[Slot]> = event.parents.iter().map(|p| p.index() as Slot).collect();
            let (input, values, output, facts, encoding) = match &step.kind {
                Kind::Input => {
                    let values = match &trace.source {
                        Source::Derived { data, .. } => match rows.len() == total {
                            true => data.clone(),
                            false => Rc::new(Data::gather(std::slice::from_ref(data), &rows)),
                        },
                        Source::Rows {
                            requests,
                            shared: true,
                        } => Rc::new(Data::plain(
                            rows.clone(),
                            rows.iter()
                                .map(|&row| requests.get(row).cloned().unwrap_or(Variable::Null))
                                .collect(),
                        )),
                        Source::Rows { shared: false, .. } => {
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
                            let shape = self.store.shape(seg, &view);
                            Rc::new(Data::plain(rows.clone(), self.captured(&shape, &rows)))
                        }
                    };
                    (
                        false,
                        values.clone(),
                        Value::Data(values),
                        Facts::None,
                        Encoding::Request,
                    )
                }
                Kind::Output => {
                    let values = trace.input(&event.parents, &rows);
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
                        true => trace.nodes(graph, &event.visible, &rows),
                        false => Vec::new(),
                    };
                    let inputs: Vec<Variable> =
                        trace.input(&event.parents, &rows).materialized().to_vec();
                    let hosted = trace_hosted(&mut self.trace, event.node, rows.len());
                    let (facts, values): (Facts<'a>, Rc<[Variable]>) = match hosted {
                        Some(hosted) if hosted.walked.len() == rows.len() => {
                            (Facts::Rows(hosted.walked), hosted.values)
                        }
                        Some(hosted) => {
                            let data = Data::plain(rows.clone(), inputs.clone().into());
                            let shape = CompiledGraph::node_shape(step, Some(&data));
                            let facts = hosted
                                .kernel
                                .ok_or("evaluated outside the batch kernels")
                                .and_then(|kernel| {
                                    kernel.facts(graph.template(), slot, shape, rows.len())
                                })
                                .unwrap_or_else(|reason| {
                                    crate::compiled::trace::reevaluated(reason, rows.len());
                                    Facts::Rows(CompiledGraph::facts_of(
                                        step,
                                        &inputs,
                                        &nodes,
                                        &self.host.extensions.context.rows(&rows),
                                    ))
                                });
                            (facts, hosted.values)
                        }
                        None => {
                            crate::compiled::trace::reevaluated(
                                "plan hosts the node row by row",
                                rows.len(),
                            );
                            let (facts, outputs) = CompiledGraph::evaluated(
                                step,
                                &inputs,
                                &nodes,
                                &self.host.extensions.context.rows(&rows),
                            );
                            (Facts::Rows(facts), outputs.into())
                        }
                    };
                    let touched = graph.template().touched(slot).is_some();
                    let encoding = match (transform.pass_through, touched) {
                        (false, _) => Encoding::Checked(false),
                        (true, true) => Encoding::Touched(None),
                        (true, false) => Encoding::Checked(true),
                    };
                    let output = Rc::new(Data::plain(rows.clone(), values));
                    (true, output.clone(), Value::Data(output), facts, encoding)
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
                    let shape = self.store.shape(seg, &own);
                    let Some(trace) = self.trace.as_ref() else {
                        return;
                    };
                    let input = trace.input(&event.parents, &rows);
                    let columned = match trace.lazy {
                        true => Store::columned(&shape, &rows, total)
                            .filter(|columned| !(transform.pass_through && columned.nulls)),
                        false => None,
                    };
                    let touched = graph.template().touched(slot).is_some();
                    let keys =
                        |facts: &Facts| match (facts, transform.pass_through, transform.looped) {
                            (Facts::Results((keys, _)), false, false) => {
                                let prefix = transform.output_path.as_deref();
                                Some(
                                    keys.iter()
                                        .map(|key| match prefix {
                                            Some(prefix) => format!("{prefix}.{key}"),
                                            None => key.to_string(),
                                        })
                                        .collect::<Vec<String>>(),
                                )
                            }
                            _ => None,
                        };
                    match columned {
                        Some(columned) => {
                            let layer = columned.layer(&rows, total);
                            let facts = match self.facts(
                                step,
                                event,
                                body,
                                transform.looped,
                                kernel,
                                &rows,
                                &input,
                                Own::Columns(&columned),
                            ) {
                                Some(facts) => facts,
                                None => {
                                    self.refuse("node reads $nodes");
                                    return;
                                }
                            };
                            let values = Rc::new(match transform.pass_through {
                                false => Data::record(&rows, layer, None),
                                true => input.layered(layer),
                            });
                            let complete = keys(&facts).map(|paths| {
                                let exact: Vec<Option<usize>> =
                                    paths.iter().map(|path| columned.at(path)).collect();
                                match exact.iter().all(Option::is_some) {
                                    true => rows
                                        .iter()
                                        .map(|&row| {
                                            exact.iter().flatten().all(|&index| {
                                                Bits::get(&columned.present[index], row)
                                            })
                                        })
                                        .collect::<Rc<[bool]>>(),
                                    false => self
                                        .captured(&shape, &rows)
                                        .iter()
                                        .map(|o| paths.iter().all(|p| o.dot(p).is_some()))
                                        .collect(),
                                }
                            });
                            let encoding = match (transform.pass_through, touched, complete) {
                                (false, _, Some(complete)) => Encoding::Results(complete),
                                (false, _, None) => Encoding::Checked(false),
                                (true, true, _) => Encoding::Touched(Some(input.clone())),
                                (true, false, _) => Encoding::Checked(true),
                            };
                            (true, values.clone(), Value::Data(values), facts, encoding)
                        }
                        None => {
                            let own = self.captured(&shape, &rows);
                            let values: Rc<[Variable]> = match transform.pass_through {
                                false => own.clone(),
                                true => input
                                    .materialized()
                                    .iter()
                                    .zip(own.iter())
                                    .map(|(before, own)| before.clone().merge_clone(own))
                                    .collect(),
                            };
                            let facts = match self.facts(
                                step,
                                event,
                                body,
                                transform.looped,
                                kernel,
                                &rows,
                                &input,
                                Own::Values(&own),
                            ) {
                                Some(facts) => facts,
                                None => {
                                    self.refuse("node reads $nodes");
                                    return;
                                }
                            };
                            let complete = keys(&facts).map(|paths| {
                                own.iter()
                                    .map(|o| paths.iter().all(|p| o.dot(p).is_some()))
                                    .collect::<Rc<[bool]>>()
                            });
                            let encoding = match (transform.pass_through, touched, complete) {
                                (false, _, Some(complete)) => Encoding::Results(complete),
                                (false, _, None) => Encoding::Checked(false),
                                (true, true, _) => Encoding::Touched(None),
                                (true, false, _) => Encoding::Checked(true),
                            };
                            let output = Rc::new(Data::plain(rows.clone(), values));
                            (true, output.clone(), Value::Data(output), facts, encoding)
                        }
                    }
                }
                Kind::Switch { .. } | Kind::Host(_) => {
                    self.refuse("hosted node");
                    return;
                }
            };
            let Some(trace) = self.trace.as_mut() else {
                return;
            };
            trace.events.push(BatchEvent {
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
                *entry = Some(values);
            }
        }
        if let Some(trace) = self.trace.as_mut() {
            trace.kernels.clear();
            trace.hosted.clear();
            trace.times.clear();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn facts(
        &self,
        step: &crate::compiled::graph::Step,
        event: &crate::compiled::graph::schedule::Execute,
        body: &Body,
        looped: bool,
        kernel: Option<KernelFacts>,
        rows: &Rc<[usize]>,
        input: &Rc<Data<'a>>,
        own: Own<'_, 'a>,
    ) -> Option<Facts<'a>> {
        let count = rows.len();
        let reason = match (looped, body) {
            (true, _) => match kernel {
                Some(kernel) => {
                    let data = Data::plain(rows.clone(), input.materialized());
                    let shape = CompiledGraph::node_shape(step, Some(&data));
                    let slot = event.node.index() as Slot;
                    match kernel.facts(self.graph.template(), slot, shape, count) {
                        Ok(facts) => return Some(facts),
                        Err(reason) => reason,
                    }
                }
                None => "evaluated outside the batch kernels",
            },
            (false, Body::Expression { keys, .. }) if CompiledGraph::keyed_results(step) => {
                let prefix = match &step.kind {
                    Kind::Node { transform, .. } => transform.output_path.clone(),
                    _ => None,
                };
                let path = |key: &Arc<str>| match &prefix {
                    Some(prefix) => format!("{prefix}.{key}"),
                    None => key.to_string(),
                };
                fn dotted<'x>(values: &Rc<[Variable]>, path: &str) -> Leaf<'x> {
                    Leaf::Any(
                        values
                            .iter()
                            .map(|o| o.dot(path).unwrap_or(Variable::Null))
                            .collect(),
                    )
                }
                let leaves = match &own {
                    Own::Values(values) => {
                        Cols::Owned(keys.iter().map(|key| dotted(values, &path(key))).collect())
                    }
                    Own::Columns(columned) => {
                        let total = self.store.rows;
                        let mut whole: Option<Rc<[Variable]>> = None;
                        let exact: Vec<Option<usize>> =
                            keys.iter().map(|key| columned.at(&path(key))).collect();
                        match exact.iter().all(Option::is_some) {
                            true => Cols::Lazy(
                                exact
                                    .iter()
                                    .flatten()
                                    .map(|&index| columned.leaf(index))
                                    .collect(),
                                (rows.len() != total).then(|| rows.clone()),
                            ),
                            false => Cols::Lazy(
                                keys.iter()
                                    .zip(&exact)
                                    .map(|(key, exact)| match exact {
                                        Some(index) => {
                                            let (leaf, present) = columned.leaf(*index);
                                            let leaf = match rows.len() == total {
                                                true => leaf,
                                                false => leaf.pick(rows),
                                            };
                                            let present = present.map(|present| {
                                                Rc::from(Bits::gather(&present, rows))
                                            });
                                            (leaf, present)
                                        }
                                        None => (
                                            dotted(
                                                whole.get_or_insert_with(|| {
                                                    Data::record(
                                                        rows,
                                                        columned.layer(rows, total),
                                                        None,
                                                    )
                                                    .materialized()
                                                }),
                                                &path(key),
                                            ),
                                            None,
                                        ),
                                    })
                                    .collect(),
                                None,
                            ),
                        }
                    }
                };
                return Some(Facts::Results((keys.clone(), leaves)));
            }
            (false, Body::Expression { .. }) => "expression without one result per key",
            (false, Body::Table(table)) => {
                let slot = event.node.index() as Slot;
                let shape = NodeShape::Table {
                    first: table.first_hit(),
                    derived: CompiledGraph::derived_refs(step),
                };
                match kernel.map(|kernel| kernel.facts(self.graph.template(), slot, shape, count)) {
                    Some(Ok(facts)) => return Some(facts),
                    Some(Err(reason)) => reason,
                    None => "evaluated outside the batch kernels",
                }
            }
        };
        if step.nodes {
            return None;
        }
        crate::compiled::trace::reevaluated(reason, count);
        let inputs = input.materialized();
        Some(Facts::Rows(CompiledGraph::facts_of(
            step,
            &inputs,
            &[],
            &self.host.extensions.context.picked(rows.clone()),
        )))
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
        let merged = trace.input(parents, &rows);
        trace.events.push(BatchEvent {
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
            *entry = Some(merged);
        }
    }
}

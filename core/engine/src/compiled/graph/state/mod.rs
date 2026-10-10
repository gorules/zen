mod exec;
mod host;
mod loops;

use crate::decision_graph::cleaner::VariableCleaner;
use crate::decision_graph::walker::GraphWalker;
use crate::model::GraphContent;
use crate::nodes::NodeError;
use crate::nodes::NodeHandlerExtensions;
use crate::{EvaluationError, ZEN_CONFIG};
use petgraph::prelude::NodeIndex;
use crate::compiled::graph::schedule::{End, Outcome, Replay, Segment};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use crate::compiled::graph::bound::{Bound, Scopes};
use crate::compiled::graph::data::{Col, Data};
use crate::compiled::leaf::{Array, Leaf};
use std::rc::Rc;
use zen_types::symbol::Symbol;
use zen_expression::lane::{Binding, Columns};
use zen_expression::{Scope, Variable};
use zen_types::decision::DecisionNode;
use crate::compiled::graph::{CompiledGraph, Condition, Hit, Kind, Step, data, plan};

pub(super) type NodeColumn<'a> = (Arc<str>, Leaf<'a>);

pub(in crate::compiled::graph) struct Host<'r> {
    pub(in crate::compiled::graph) content: &'r Arc<GraphContent>,
    pub(in crate::compiled::graph) extensions: &'r NodeHandlerExtensions,
    pub(in crate::compiled::graph) max_depth: u8,
}

pub(crate) struct State<'a> {
    nodes: Vec<Vec<Rc<Data<'a>>>>,
    pub(crate) errors: Vec<Option<Box<EvaluationError>>>,
    failed: usize,
    pub(in crate::compiled::graph) results: Vec<Option<Variable>>,
    pub(crate) endings: Vec<(Rc<[usize]>, Vec<Data<'a>>)>,
    nesting: Nesting<'a>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Nesting<'a> {
    pub(in crate::compiled::graph) parents: Option<&'a [Option<Variable>]>,
    pub(in crate::compiled::graph) iteration: u8,
}

pub(crate) enum Source<'a> {
    Rows(&'a [Variable]),
    Columns(Rc<Data<'a>>),
}

impl<'a> Source<'a> {
    fn input(&self, rows: &Rc<[usize]>) -> Data<'a> {
        match self {
            Source::Rows(inputs) => Data::plain(rows.clone(), rows.iter().map(|&r| inputs[r].clone()).collect()),
            Source::Columns(data) => Data::gather(std::slice::from_ref(data), rows),
        }
    }
}

impl CompiledGraph {
    pub(in crate::compiled::graph) fn child(&self, segment: &Segment, outcome: Outcome) -> Arc<Segment> {
        let End::Switch { children, .. } = &segment.end else {
            return self.root.clone();
        };
        children.get(&outcome, || {
            let mut path = segment.path.to_vec();
            path.push(outcome.clone());
            Arc::new(Replay::new(&self.graph).segment(&path))
        })
    }

    fn node_columns<'a>(&self, step: &Step, state: &State<'a>, visible: &[NodeIndex], rows: &Rc<[usize]>, with_nodes: bool) -> Vec<NodeColumn<'a>> {
        let Some(statics) = &step.statics else {
            return Vec::new();
        };
        statics
            .iter()
            .map(|(key, name, path)| {
                let source = visible
                    .iter()
                    .rev()
                    .filter(|_| with_nodes)
                    .find(|nid| self.step_at(**nid).is_some_and(|s| s.node.name.as_ref() == name.as_ref()))
                    .and_then(|nid| state.nodes.get(nid.index()))
                    .filter(|pieces| !pieces.is_empty())
                    .map(|pieces| Data::gather(pieces, rows));
                let leaf = match source {
                    Some(data) if path.is_empty() => Leaf::Any(data.materialized()),
                    Some(data) => data.column_at(path),
                    None => Leaf::nulls(rows.len()),
                };
                (key.clone(), leaf)
            })
            .collect()
    }

    pub(in crate::compiled::graph) fn step_at(&self, nid: NodeIndex) -> Option<&Step> {
        self.steps.get(nid.index())?.as_ref()
    }

    fn input_of<'a>(state: &State<'a>, parents: &[NodeIndex], rows: &Rc<[usize]>) -> Data<'a> {
        let gathered: Vec<Data<'a>> = parents
            .iter()
            .filter_map(|p| {
                let pieces = state.nodes.get(p.index())?;
                (!pieces.is_empty()).then(|| Data::gather(pieces, rows))
            })
            .collect();
        Data::merged(rows, gathered)
    }

    fn nodes_of(&self, state: &State, visible: &[NodeIndex], rows: &Rc<[usize]>) -> Vec<Variable> {
        let columns: Vec<(Arc<str>, Col)> = visible
            .iter()
            .filter_map(|nid| {
                let step = self.step_at(*nid)?;
                let pieces = state.nodes.get(nid.index()).filter(|pieces| !pieces.is_empty())?;
                let column = Data::gather(pieces, rows).materialized();
                let column = match (&step.kind, state.nesting.parents) {
                    (Kind::Input, Some(parents)) => column
                        .iter()
                        .zip(rows.iter())
                        .map(|(value, &row)| match parents.get(row).cloned().flatten() {
                            Some(parent) => {
                                let view = value.depth_clone(1);
                                view.dot_insert(Variable::nodes_key().as_ref(), parent);
                                view
                            }
                            None => value.clone(),
                        })
                        .collect(),
                    _ => column,
                };
                Some((step.node.name.clone(), column))
            })
            .collect();
        (0..rows.len())
            .map(|row| {
                let object = Variable::empty_object();
                if let Variable::Object(map) = &object {
                    let mut map = map.borrow_mut();
                    for (name, column) in &columns {
                        map.insert(name.as_ref().into(), column[row].clone());
                    }
                }
                object
            })
            .collect()
    }

    pub(in crate::compiled::graph) fn scope(input: &Variable, nodes: Option<&Variable>) -> Scope {
        let mut scope = Scope::new(input.clone());
        if let Some(nodes) = nodes {
            scope.set_local(Variable::nodes_key(), nodes.clone());
        }
        scope
    }

    fn fault(state: &mut State, row: usize, error: NodeError) {
        state.failed += 1;
        state.errors[row] = Some(Box::new(EvaluationError::NodeError {
            node_id: error.node_id,
            source: error.source,
            trace: None,
        }));
    }

    fn fail(state: &mut State, row: usize, node: &DecisionNode, message: String) {
        state.failed += 1;
        state.errors[row] = Some(Box::new(EvaluationError::NodeError {
            node_id: node.id.clone(),
            source: message.into(),
            trace: None,
        }));
    }

    fn alive(state: &State, rows: &Rc<[usize]>) -> Rc<[usize]> {
        if state.failed == 0 {
            return rows.clone();
        }
        match rows.iter().all(|&r| state.errors[r].is_none()) {
            true => rows.clone(),
            false => rows.iter().copied().filter(|&r| state.errors[r].is_none()).collect(),
        }
    }

    pub(in crate::compiled::graph) async fn run<'a>(&self, host: Host<'_>, source: &Source<'a>, count: usize, columnar: bool, nesting: Nesting<'a>) -> State<'a> {
        let Host { content, extensions, max_depth } = host;
        let mut state = State {
            nodes: vec![Vec::new(); self.steps.len()],
            errors: (0..count).map(|_| None).collect(),
            results: vec![None; count],
            endings: Vec::new(),
            failed: 0,
            nesting,
        };
        if nesting.iteration >= max_depth {
            for error in state.errors.iter_mut() {
                *error = Some(Box::new(EvaluationError::DepthLimitExceeded));
            }
            return state;
        }
        let with_nodes = ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed);
        let mut stack: Vec<(Arc<Segment>, Rc<[usize]>)> = vec![(self.root.clone(), (0..count).collect())];
        while let Some((segment, mut rows)) = stack.pop() {
            for event in segment.events.iter() {
                rows = Self::alive(&state, &rows);
                if rows.is_empty() {
                    break;
                }
                match self.step_at(event.node).filter(|step| Self::immediate(step)) {
                    Some(step) => self.execute_now(step, event, &rows, source, &mut state, with_nodes),
                    None => {
                        self.execute(event, &rows, source, &mut state, content, extensions, with_nodes, max_depth)
                            .await
                    }
                }
            }
            rows = Self::alive(&state, &rows);
            if rows.is_empty() {
                continue;
            }
            match &segment.end {
                End::Finish(ending) if columnar => {
                    let datas: Vec<Data<'a>> = ending
                        .iter()
                        .filter_map(|nid| {
                            let pieces = state.nodes.get(nid.index())?;
                            (!pieces.is_empty()).then(|| Data::gather(pieces, &rows))
                        })
                        .collect();
                    state.endings.push((rows.clone(), datas));
                }
                End::Finish(ending) => {
                    let columns: Vec<Col> = ending
                        .iter()
                        .filter_map(|nid| {
                            let pieces = state.nodes.get(nid.index())?;
                            (!pieces.is_empty()).then(|| Data::gather(pieces, &rows).materialized())
                        })
                        .collect();
                    for (i, &row) in rows.iter().enumerate() {
                        let result = match columns.as_slice() {
                            [single] => Self::ending(&single[i]),
                            _ => GraphWalker::merge_ending(columns.iter().map(|c| &c[i])),
                        };
                        if state.nesting.parents.is_none() {
                            VariableCleaner::new().clean(&result);
                        }
                        state.results[row] = Some(result);
                    }
                }
                End::Switch {
                    node,
                    parents,
                    visible,
                    ..
                } => {
                    let groups = self.switch(*node, parents, visible, &rows, &state, with_nodes);
                    for (outcome, members) in groups.into_iter().rev() {
                        let members: Rc<[usize]> = match members.len() == rows.len() {
                            true => rows.clone(),
                            false => members.into(),
                        };
                        stack.push((self.child(&segment, outcome), members));
                    }
                }
            }
        }
        state
    }

    fn ending(value: &Variable) -> Variable {
        if let Variable::Object(map) = value {
            if Rc::strong_count(map) == 1 && map.borrow().shape().is_some() {
                let nulls: Vec<Symbol> = map
                    .borrow()
                    .iter()
                    .filter(|(_, v)| matches!(v, Variable::Null))
                    .map(|(k, _)| k.clone())
                    .collect();
                let mut map = map.borrow_mut();
                for key in &nulls {
                    map.remove(key);
                }
                return value.clone();
            }
        }
        GraphWalker::merge_ending(std::iter::once(value))
    }

    fn node_inputs<'a>(
        &self,
        step: &Step,
        state: &State<'a>,
        visible: &[NodeIndex],
        rows: &Rc<[usize]>,
        with_nodes: bool,
        columnar: bool,
    ) -> (Vec<Option<Variable>>, Vec<NodeColumn<'a>>) {
        match (step.nodes, columnar && step.statics.is_some() && state.nesting.parents.is_none()) {
            (false, _) => (Vec::new(), Vec::new()),
            (true, true) => (Vec::new(), self.node_columns(step, state, visible, rows, with_nodes)),
            (true, false) => match with_nodes {
                true => (self.nodes_of(state, visible, rows).into_iter().map(Some).collect(), Vec::new()),
                false => (Vec::new(), Vec::new()),
            },
        }
    }

    fn whole(step: &Step, data: &Data, rebase: Option<&str>) -> bool {
        let leaves = data.leaves();
        if leaves.is_empty() {
            return false;
        }
        (step.root && rebase.is_none())
            || step.dynamic
            || leaves
                .iter()
                .any(|(k, _)| k.as_ref() == "$" || k.as_ref() == "$nodes" || k.starts_with("$nodes."))
    }

    fn partials<'a>(step: &Step, data: &Data<'a>) -> Vec<NodeColumn<'a>> {
        let mut out: Vec<NodeColumn> = Vec::new();
        for path in step.sites.iter().flatten() {
            if data.binding(path).1 && !out.iter().any(|(p, _)| p == path) {
                out.push((path.clone(), data.partial(path)));
            }
        }
        out
    }

    fn bound<'a, R>(
        step: &Step,
        data: &Data<'a>,
        nodes: &[Option<Variable>],
        extra: &[NodeColumn<'a>],
        rebase: Option<&str>,
        f: impl FnOnce(&Bound) -> R,
    ) -> R {
        let with_nodes = |scope: Scope, row: usize| match nodes.get(row).cloned().flatten() {
            None => scope,
            Some(n) => {
                let mut scope = scope;
                scope.set_local(Variable::nodes_key(), n.clone());
                scope
            }
        };
        let root = |values: Col| -> Col {
            match rebase {
                None => values,
                Some(base) => values
                    .iter()
                    .map(|v| Data::lookup(v, base).unwrap_or(Variable::Null))
                    .collect(),
            }
        };
        if Self::whole(step, data, rebase) {
            plan::census::Census::mark("whole");
            let whole = match rebase {
                None => data.materialized(),
                Some(base) => data.subtree(base),
            };
            let scopes = whole
                .iter()
                .enumerate()
                .map(|(row, v)| with_nodes(Scope::new(v.clone()), row))
                .collect();
            return f(&Bound::plain(scopes));
        }
        let blank = nodes.iter().all(Option::is_none)
            && matches!(&data.shape, data::Shape::Patched { base: None, .. });
        let scopes = match blank {
            true => Bound::blank(data.len()),
            false => Scopes::Owned(
                root(data.base())
                    .iter()
                    .enumerate()
                    .map(|(row, v)| with_nodes(Scope::new(v.clone()), row))
                    .collect(),
            ),
        };
        let leaves = data.leaves();
        let mut slots: Vec<Option<usize>> = vec![None; leaves.len()];
        let mut columns = Columns::new(data.len());
        for path in step.sites.iter().flatten() {
            if let Binding::Column(index) = data.binding(path).0 {
                if let (Some(slot @ None), Some((leaf, column))) = (slots.get_mut(index), leaves.get(index)) {
                    *slot = Some(columns.columns.len());
                    columns = columns.column(leaf.as_ref(), column.column());
                }
            }
        }
        let partials = Self::partials(step, data);
        let offset = columns.columns.len();
        for (path, leaf) in extra.iter().chain(partials.iter()) {
            columns = columns.column(path.as_ref(), leaf.column());
        }
        let names: Vec<Arc<str>> = extra.iter().chain(partials.iter()).map(|(p, _)| p.clone()).collect();
        let bound = Bound {
            scopes,
            columns,
            bind: Box::new(move |key: &str| {
                let rebased = rebase.and_then(|base| Self::rebased(base, key));
                let path = rebased.as_deref().unwrap_or(key);
                if let Some(at) = names.iter().position(|p| p.as_ref() == path) {
                    return Binding::Column(offset + at);
                }
                match data.binding(path).0 {
                    Binding::Column(index) => match slots.get(index).copied().flatten() {
                        Some(slot) => Binding::Column(slot),
                        None => Binding::Row,
                    },
                    other => other,
                }
            }),
        };
        f(&bound)
    }

    fn switch(
        &self,
        node: NodeIndex,
        parents: &[NodeIndex],
        visible: &[NodeIndex],
        rows: &Rc<[usize]>,
        state: &State<'_>,
        with_nodes: bool,
    ) -> Vec<(Outcome, Vec<usize>)> {
        let Some(step @ Step {
            kind: Kind::Switch { first, conditions },
            ..
        }) = self.step_at(node)
        else {
            return vec![(Outcome::from(Vec::new()), rows.to_vec())];
        };
        let data = Self::input_of(state, parents, rows);
        let (nodes, extra) = self.node_inputs(step, state, visible, rows, with_nodes, true);
        let hits: Vec<Hit> = Self::RUNNER.with_borrow_mut(|runner| {
            Self::bound(step, &data, &nodes, &extra, None, |bound| {
                conditions
                    .iter()
                    .map(|(_, condition)| match condition {
                        Condition::Program(program) => {
                            let p = program.program();
                            let rows = bound.rows();
                            let uniform = rows > 1
                                && !p.opaque()
                                && p.stable()
                                && p.site_keys.iter().all(|key| key.as_deref().is_some_and(|key| (bound.bind)(key) == Binding::Absent));
                            let mut outs = Vec::new();
                            let subset = [0usize];
                            bound.export(runner, program, uniform.then_some(&subset[..]), &mut outs, |_, _, _| {});
                            match (outs.first_mut().map(|out| Leaf::typed(Array::from_output(out))), uniform) {
                                (Some(column), true) => Hit::Uniform(column.truthy(0)),
                                (Some(column), false) => Hit::Rows(column.truths(rows)),
                                (None, _) => Hit::Uniform(false),
                            }
                        }
                        Condition::Always => Hit::Uniform(true),
                        Condition::Never => Hit::Uniform(false),
                    })
                    .collect()
            })
        });
        let words = conditions.len().div_ceil(64).max(1);
        let mut groups: Vec<(Vec<u64>, Vec<usize>)> = Vec::new();
        let mut key = vec![0u64; words];
        let uniform = hits.iter().all(|hit| matches!(hit, Hit::Uniform(_)));
        let lanes = match uniform {
            true => rows.len().min(1),
            false => rows.len(),
        };
        for (lane, &row) in rows.iter().enumerate().take(lanes) {
            key.iter_mut().for_each(|w| *w = 0);
            for (index, hit) in hits.iter().enumerate() {
                let on = match hit {
                    Hit::Uniform(on) => *on,
                    Hit::Rows(truths) => truths[lane >> 6] >> (lane & 63) & 1 == 1,
                };
                if on {
                    key[index / 64] |= 1 << (index % 64);
                    if *first {
                        break;
                    }
                }
            }
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push(row),
                None => groups.push((key.clone(), vec![row])),
            }
        }
        if let ([(_, members)], true) = (groups.as_mut_slice(), uniform) {
            *members = rows.to_vec();
        }
        groups
            .into_iter()
            .map(|(key, members)| {
                let valid: Vec<Arc<str>> = conditions
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| key[index / 64] >> (index % 64) & 1 == 1)
                    .map(|(_, (id, _))| id.clone())
                    .collect();
                (Outcome::from(valid), members)
            })
            .collect()
    }
}

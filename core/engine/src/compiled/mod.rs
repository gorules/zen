mod bound;
mod plan;
pub(crate) mod policy;
pub use columnar::{ColumnarOutput, OutputColumn, RecordsView};
mod schedule;
mod table;
mod columnar;
mod data;
mod schema;
mod shred;
mod typed;

use crate::decision_graph::cleaner::VariableCleaner;
use crate::decision_graph::graph::{DecisionGraph, DecisionGraphResponse};
use crate::decision_graph::schema_dict;
use crate::decision_graph::walker::{GraphWalker, StableDiDecisionGraph};
use crate::model::{DecisionNodeKind, GraphContent};
use crate::nodes::custom::CustomNodeHandler;
use crate::nodes::decision_table::DecisionTableNodeHandler;
use crate::nodes::expression::ExpressionNodeHandler;
use crate::nodes::decision::DecisionNodeHandler;
use crate::nodes::function::v2::FunctionV2NodeHandler;
use crate::nodes::function::FunctionNodeHandler;
use crate::nodes::input::dates::DeclaredDates;
use crate::nodes::NodeError;
use crate::nodes::validator_cache::ValidatorCache;
use crate::nodes::variable_json::{Guards, VariableJson, VariableNode};
use jsonschema::Validator;
use crate::nodes::input::InputNodeHandler;
use crate::nodes::output::OutputNodeHandler;
use crate::nodes::{NodeContextBase, NodeContextConfig, NodeHandlerExtensions};
use crate::{EvaluationError, ZEN_CONFIG};
use petgraph::algo::is_cyclic_directed;
use petgraph::prelude::NodeIndex;
use schedule::{End, Execute, Outcome, Replay, Segment};
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use table::{Table, TableOut};
use bound::{Bound, Scopes};
use data::{Col, Data, Layer, Mask};
use typed::{Array, ColumnBuilder, Leaf};
use std::rc::Rc;
use zen_types::symbol::Symbol;
use zen_expression::lane::{Binding, Columns};
use zen_expression::lane::{LaneProgram, LaneRunner, SourceInfo};
use zen_expression::{Isolate, Scope, Variable};
use zen_types::decision::{
    DecisionNode, DecisionNodeContent, FunctionNodeContent, InputNodeContent, OutputNodeContent, SwitchStatementHitPolicy,
    TransformAttributes, TransformExecutionMode,
};

enum Body {
    Expression {
        program: Box<LaneProgram>,
        empty: Box<[bool]>,
        entries: Box<[(Arc<str>, Arc<str>)]>,
        fragments: Box<[Arc<str>]>,
        overlapping: bool,
        walked: bool,
        keys: Arc<[Arc<str>]>,
    },
    Table(Box<Table>),
}

enum Hit {
    Uniform(bool),
    Rows(Vec<u64>),
}

struct Transform {
    input_field: Option<(LaneProgram, Arc<str>)>,
    base: Option<Arc<str>>,
    looped: bool,
    output_path: Option<Arc<str>>,
    pass_through: bool,
}

enum Kind {
    Input,
    Output,
    Switch {
        first: bool,
        conditions: Box<[(Arc<str>, Condition)]>,
    },
    Node {
        body: Body,
        transform: Box<Transform>,
    },
    Host,
}

enum Condition {
    Always,
    Never,
    Program(Box<LaneProgram>),
}

struct Step {
    node: Arc<DecisionNode>,
    kind: Kind,
    nodes: bool,
    statics: Option<Vec<StaticRead>>,
    root: bool,
    schema: bool,
    sites: Vec<Option<Arc<str>>>,
    dynamic: bool,
}

pub struct CompiledGraph {
    graph: StableDiDecisionGraph,
    steps: Vec<Option<Step>>,
    root: Arc<Segment>,
    dictionaries: bool,
    plan: Option<plan::Plan>,
}

pub struct CompiledPlan(Result<CompiledGraph, Arc<str>>);

impl Debug for CompiledPlan {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Ok(_) => write!(f, "CompiledPlan(Ok)"),
            Err(reason) => write!(f, "CompiledPlan(Err({reason}))"),
        }
    }
}

impl PartialEq for CompiledPlan {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl CompiledPlan {
    pub fn compile(content: &GraphContent) -> Self {
        Self(CompiledGraph::compile(content).map_err(Arc::from))
    }

    pub fn verdict(&self) -> Result<(), &str> {
        self.0.as_ref().map(|_| ()).map_err(|reason| reason.as_ref())
    }

    pub(crate) fn usable(&self, content: &GraphContent) -> Option<&CompiledGraph> {
        let graph = self.0.as_ref().ok()?;
        (!graph.dictionaries || content.resolved_schemas.is_some()).then_some(graph)
    }
}

type StaticRead = (Arc<str>, Arc<str>, Arc<str>);
type NodeColumn<'a> = (Arc<str>, Leaf<'a>);

pub(crate) struct State<'a> {
    nodes: Vec<Vec<Rc<Data<'a>>>>,
    pub(crate) errors: Vec<Option<Box<EvaluationError>>>,
    failed: usize,
    results: Vec<Option<Variable>>,
    pub(crate) endings: Vec<(Rc<[usize]>, Vec<Data<'a>>)>,
    nesting: Nesting<'a>,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Nesting<'a> {
    parents: Option<&'a [Option<Variable>]>,
    iteration: u8,
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
    thread_local! {
        static RUNNER: RefCell<LaneRunner> = RefCell::new(LaneRunner::new());
    }

    fn compile(content: &GraphContent) -> Result<Self, String> {
        let graph = DecisionGraph::build_graph(content).map_err(|e| e.to_string())?;
        let inputs = graph
            .node_weights()
            .filter(|w| matches!(w.kind, DecisionNodeKind::InputNode { .. }))
            .count();
        if inputs != 1 || is_cyclic_directed(&graph) {
            return Err("graph fails validation".into());
        }

        let mut dictionaries = false;
        let mut steps: Vec<Option<Step>> = Vec::new();
        for nid in graph.node_indices() {
            let node = graph[nid].clone();
            let step = Self::step(&node, &mut dictionaries)?;
            let index = nid.index();
            if steps.len() <= index {
                steps.resize_with(index + 1, || None);
            }
            steps[index] = Some(step);
        }

        let root = Arc::new(Replay::new(&graph).segment(&[]));
        let mut compiled = Self {
            graph,
            steps,
            root,
            dictionaries,
            plan: None,
        };
        if std::env::var("ZEN_LEGACY").is_err() {
            compiled.plan = plan::Plan::analyze(&compiled).ok();
        }
        Ok(compiled)
    }

    pub(crate) fn plan_verdict(&self, columns: &Columns) -> Result<(), String> {
        match &self.plan {
            Some(plan) => plan.verdict(self, columns),
            None => Err(self
                .steps
                .iter()
                .flatten()
                .find_map(|step| plan::Plan::analyze_step(step).err())
                .unwrap_or_else(|| "refused".into())),
        }
    }

    fn transform(attributes: &TransformAttributes) -> Result<(Transform, Vec<Arc<str>>), String> {
        let input_field = attributes
            .input_field
            .as_ref()
            .map(|f| {
                LaneProgram::standard(f)
                    .map(|p| (p, f.clone()))
                    .map_err(|e| e.to_string())
            })
            .transpose()?;
        let looped = matches!(attributes.execution_mode, TransformExecutionMode::Loop);
        let base = input_field
            .as_ref()
            .filter(|_| !looped)
            .and_then(|(program, _)| program.path().map(Arc::from));
        Ok((
            Transform {
                input_field,
                base,
                looped,
                output_path: attributes.output_path.clone(),
                pass_through: attributes.pass_through,
            },
            attributes.input_field.iter().cloned().collect(),
        ))
    }

    fn step(node: &Arc<DecisionNode>, dictionaries: &mut bool) -> Result<Step, String> {
        let mut sources: Vec<Arc<str>> = Vec::new();
        let mut cells: Vec<Arc<str>> = Vec::new();
        let kind = match &node.kind {
            DecisionNodeKind::InputNode { content } => {
                *dictionaries |= content
                    .schema
                    .as_deref()
                    .is_some_and(schema_dict::schema_references_dictionary);
                Kind::Input
            }
            DecisionNodeKind::OutputNode { content } => {
                *dictionaries |= content
                    .schema
                    .as_deref()
                    .is_some_and(schema_dict::schema_references_dictionary);
                Kind::Output
            }
            DecisionNodeKind::SwitchNode { content } => {
                let conditions = content
                    .statements
                    .iter()
                    .map(|s| {
                        sources.push(s.condition.clone());
                        let condition = match s.condition.is_empty() {
                            true => Condition::Always,
                            false => LaneProgram::standard(&s.condition)
                                .map(|p| Condition::Program(Box::new(p)))
                                .unwrap_or(Condition::Never),
                        };
                        (s.id.clone(), condition)
                    })
                    .collect();
                Kind::Switch {
                    first: matches!(content.hit_policy, SwitchStatementHitPolicy::First),
                    conditions,
                }
            }
            DecisionNodeKind::ExpressionNode { content } => {
                let entries: Box<[(Arc<str>, Arc<str>)]> = content
                    .expressions
                    .iter()
                    .filter(|e| !e.key.is_empty() && !e.value.is_empty())
                    .map(|e| (e.key.clone(), e.value.clone()))
                    .collect();
                sources.extend(entries.iter().map(|(_, v)| v.clone()));
                let opaque = entries.iter().any(|(_, v)| {
                    v.match_indices('$').any(|(at, _)| {
                        let rest = &v[at + 1..];
                        !rest.starts_with("nodes") && !rest.starts_with('.') && !rest.starts_with('{')
                    })
                });
                let overlapping = entries.iter().any(|(key, _)| key.split('.').any(str::is_empty))
                    || entries.iter().enumerate().any(|(i, (a, _))| {
                    entries.iter().skip(i + 1).any(|(b, _)| {
                        a == b
                            || a.strip_prefix(b.as_ref()).is_some_and(|r| r.starts_with('.'))
                            || b.strip_prefix(a.as_ref()).is_some_and(|r| r.starts_with('.'))
                    })
                });
                let walked = overlapping && entries.iter().any(|(_, source)| source.contains('$'));
                let mut expanded: Vec<(Arc<str>, Arc<str>, Arc<str>)> = Vec::with_capacity(entries.len());
                for (key, source) in entries.iter() {
                    let read = format!("$.{}", key.split('.').next().unwrap_or_default());
                    let flat = !opaque && !overlapping && !entries.iter().any(|(_, v)| v.contains(read.as_str()));
                    match flat {
                        true => Self::expand(key, source, source, &mut expanded),
                        false => expanded.push((key.clone(), source.clone(), source.clone())),
                    }
                }
                let refs: Vec<(&str, &str)> = expanded.iter().map(|(k, _, v)| (k.as_ref(), v.as_ref())).collect();
                let program = LaneProgram::compile_many(&refs, true).map_err(|e| e.to_string())?;
                let empty: Box<[bool]> = expanded.iter().map(|(_, _, fragment)| fragment.trim() == "{}").collect();
                let fragments: Box<[Arc<str>]> = expanded.iter().map(|(_, _, fragment)| fragment.clone()).collect();
                let entries: Box<[(Arc<str>, Arc<str>)]> =
                    expanded.into_iter().map(|(key, source, _)| (key, source)).collect();
                let (transform, extra) = Self::transform(&content.transform_attributes)?;
                sources.extend(extra);
                Kind::Node {
                    transform: Box::new(transform),
                    body: Body::Expression {
                        program: Box::new(program),
                        keys: entries.iter().map(|(k, _)| k.clone()).collect(),
                        entries,
                        fragments,
                        empty,
                        overlapping,
                        walked,
                    },
                }
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                for rule in content.rules.iter() {
                    for input in content.inputs.iter() {
                        if let Some(cell) = rule.get(&input.id) {
                            match input.field.is_some() {
                                true => cells.push(cell.clone()),
                                false => sources.push(cell.clone()),
                            }
                        }
                    }
                    for output in content.outputs.iter() {
                        sources.extend(rule.get(&output.id).cloned());
                    }
                }
                sources.extend(content.inputs.iter().filter_map(|i| i.field.clone()));
                let table = Table::compile(content)?;
                let (transform, extra) = Self::transform(&content.transform_attributes)?;
                sources.extend(extra);
                Kind::Node {
                    body: Body::Table(Box::new(table)),
                    transform: Box::new(transform),
                }
            }
            DecisionNodeKind::FunctionNode { .. }
            | DecisionNodeKind::DecisionNode { .. }
            | DecisionNodeKind::CustomNode { .. } => Kind::Host,
        };
        let mut kind = kind;
        if let Kind::Node { body, transform } = &mut kind {
            if let Some(base) = transform.base.clone() {
                let mappable = match &*body {
                    Body::Expression { program, .. } => program
                        .program()
                        .site_keys
                        .iter()
                        .flatten()
                        .all(|key| Self::rebased(&base, key).is_some()),
                    Body::Table(_) => false,
                };
                if !mappable {
                    transform.base = None;
                }
            }
        }
        let mut programs: Vec<&LaneProgram> = Vec::new();
        match &kind {
            Kind::Switch { conditions, .. } => programs.extend(conditions.iter().filter_map(|(_, c)| match c {
                Condition::Program(p) => Some(p.as_ref()),
                _ => None,
            })),
            Kind::Node { body, transform } => {
                match body {
                    Body::Expression { program, .. } => programs.push(program),
                    Body::Table(table) => programs.extend(table.programs()),
                }
                programs.extend(transform.input_field.iter().map(|(p, _)| p));
            }
            Kind::Input | Kind::Output | Kind::Host => {}
        }
        let base = match &kind {
            Kind::Node { transform, .. } => transform.base.clone(),
            _ => None,
        };
        let input_sites = match &kind {
            Kind::Node { transform, .. } => transform.input_field.iter().count(),
            _ => 0,
        };
        let body_programs = programs.len() - input_sites;
        let programs = match base {
            Some(_) => &programs[..body_programs],
            None => &programs[..],
        };
        let sites: Vec<Option<Arc<str>>> = programs
            .iter()
            .enumerate()
            .flat_map(|(index, p)| {
                let base = base.as_deref().filter(|_| index < body_programs);
                p.program().site_keys.iter().map(move |k| {
                    k.as_deref().map(|key| match base {
                        Some(base) => Self::rebased(base, key).unwrap_or_else(|| Arc::from(key)),
                        None => Arc::from(key),
                    })
                })
            })
            .collect();
        let dynamic = programs
            .iter()
            .any(|p| p.program().writes_env || p.program().chain || p.program().opaque());
        let schema = match &node.kind {
            DecisionNodeKind::InputNode { content } => content.schema.is_some(),
            DecisionNodeKind::OutputNode { content } => content.schema.is_some(),
            _ => false,
        };
        Ok(Step {
            node: node.clone(),
            statics: match &kind {
                Kind::Host => None,
                Kind::Node { body: Body::Table(_), .. } => None,
                _ if sources.iter().any(|s| SourceInfo::reads_root(s)) => None,
                _ => Self::statics(&sites),
            },
            nodes: matches!(kind, Kind::Host)
                || sources.iter().any(|s| SourceInfo::reads_nodes(s))
                || cells.iter().any(|s| SourceInfo::reads_nodes_unary(s)),
            root: match &kind {
                Kind::Node {
                    body: Body::Expression { program, .. },
                    transform,
                } => {
                    program.program().keys.iter().any(|k| k == "$" || k.starts_with("$."))
                        || transform.input_field.as_ref().is_some_and(|(_, source)| SourceInfo::reads_root(source))
                }
                _ => {
                    sources.iter().any(|s| SourceInfo::reads_root(s))
                        || cells.iter().any(|s| SourceInfo::reads_root_unary(s))
                }
            },
            schema,
            sites,
            dynamic,
            kind,
        })
    }

    fn child(&self, segment: &Segment, outcome: Outcome) -> Arc<Segment> {
        let End::Switch { children, .. } = &segment.end else {
            return self.root.clone();
        };
        if let Some(found) = children.read().ok().and_then(|c| c.get(&outcome).cloned()) {
            return found;
        }
        let mut path = segment.path.to_vec();
        path.push(outcome.clone());
        let built = Arc::new(Replay::new(&self.graph).segment(&path));
        match children.write() {
            Ok(mut map) => map.entry(outcome).or_insert(built).clone(),
            Err(_) => built,
        }
    }

    fn expand(key: &Arc<str>, origin: &Arc<str>, source: &str, out: &mut Vec<(Arc<str>, Arc<str>, Arc<str>)>) {
        match LaneProgram::object_fields(source).filter(|fields| !fields.is_empty()) {
            Some(fields) => {
                for (field, fragment) in fields.into_iter().rev() {
                    Self::expand(&Arc::from(format!("{key}.{field}")), origin, &fragment, out);
                }
            }
            None => out.push((key.clone(), origin.clone(), Arc::from(source))),
        }
    }

    fn rebased(base: &str, key: &str) -> Option<Arc<str>> {
        if key.starts_with("$nodes") {
            return Some(Arc::from(key));
        }
        match key.strip_prefix('$') {
            Some("") => Some(Arc::from(base)),
            Some(rest) if rest.starts_with('.') => Some(Arc::from(format!("{base}{rest}"))),
            Some(_) => None,
            None => Some(Arc::from(format!("{base}.{key}"))),
        }
    }

    fn statics(sites: &[Option<Arc<str>>]) -> Option<Vec<StaticRead>> {
        if sites.iter().any(Option::is_none) {
            return None;
        }
        let mut out: Vec<StaticRead> = Vec::new();
        for key in sites.iter().flatten() {
            if key.as_ref() == "$nodes" {
                return None;
            }
            let Some(rest) = key.strip_prefix("$nodes.") else {
                continue;
            };
            let (name, path) = rest.split_once('.').unwrap_or((rest, ""));
            if !out.iter().any(|(k, _, _)| k == key) {
                out.push((key.clone(), Arc::from(name), Arc::from(path)));
            }
        }
        Some(out)
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

    fn step_at(&self, nid: NodeIndex) -> Option<&Step> {
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

    fn scope(input: &Variable, nodes: Option<&Variable>) -> Scope {
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

    pub(crate) async fn run<'a>(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        source: &Source<'a>,
        count: usize,
        columnar: bool,
        nesting: Nesting<'a>,
    ) -> State<'a> {
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

    pub(crate) async fn evaluate(
        &self,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        inputs: &[Variable],
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let start = Instant::now();
        let state = self
            .run(content, extensions, max_depth, &Source::Rows(inputs), inputs.len(), false, Nesting::default())
            .await;
        let performance = format!("{:.1?}", start.elapsed());
        state
            .errors
            .into_iter()
            .zip(state.results)
            .map(|(error, result)| match error {
                Some(error) => Err(error),
                None => Ok(DecisionGraphResponse {
                    performance: performance.clone(),
                    result: result.unwrap_or_else(Variable::empty_object),
                    trace: None,
                }),
            })
            .collect()
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

    #[allow(clippy::too_many_arguments)]
    async fn execute<'a>(
        &self,
        event: &Execute,
        rows: &Rc<[usize]>,
        source: &Source<'a>,
        state: &mut State<'a>,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
        with_nodes: bool,
        max_depth: u8,
    ) {
        let Some(step) = self.step_at(event.node) else {
            return;
        };
        let data = match step.kind {
            Kind::Input => source.input(rows),
            _ => Self::input_of(state, &event.parents, rows),
        };
        let column_path = match &step.kind {
            Kind::Node { body, transform } => {
                (transform.input_field.is_none() || transform.base.is_some())
                    && !transform.looped
                    && (rows.len() > 1 || matches!(source, Source::Columns(_)))
                    && !(transform.output_path.is_some() && matches!(body, Body::Expression { keys, .. } if keys.is_empty()))
            }
            _ => false,
        };
        let (nodes, extra) = self.node_inputs(step, state, &event.visible, rows, with_nodes, column_path);

        if matches!(step.kind, Kind::Host) {
            let materialized = data.materialized();
            let results = Self::hosted(step, &materialized, &nodes, extensions, max_depth, state.nesting.iteration, state.errors.len() > 1).await;
            let mut values = Vec::with_capacity(rows.len());
            for (i, result) in results.into_iter().enumerate() {
                match result {
                    Ok(v) => values.push(v),
                    Err(error) => {
                        Self::fault(state, rows[i], error);
                        values.push(Variable::Null);
                    }
                }
            }
            state.nodes[event.node.index()].push(Rc::new(Data::plain(rows.clone(), values.into())));
            return;
        }

        let (output, failures): (Data<'a>, Vec<(usize, String)>) = match &step.kind {
            Kind::Switch { .. } | Kind::Host => (data, Vec::new()),
            Kind::Output if !step.schema => (data, Vec::new()),
            Kind::Input if matches!(source, Source::Columns(_)) && Self::columnar_input(step, content, extensions) => {
                let failures = self.validate_input(step, &data, &nodes, content, extensions).await;
                (data, failures)
            }
            Kind::Input if !step.schema && Self::unresolved(content, &step.node) => {
                let values: Col = data
                    .materialized()
                    .iter()
                    .map(|value| DeclaredDates::prepare(value, None).unwrap_or_else(|| value.clone()))
                    .collect();
                (Data::plain(rows.clone(), values), Vec::new())
            }
            Kind::Input | Kind::Output => {
                let (values, failures) = self.bounded(step, &data.materialized(), &nodes, content, extensions).await;
                (Data::plain(rows.clone(), values.into()), failures)
            }
            Kind::Node { .. } => self.node(step, rows, source, &data, &nodes, &extra),
        };

        for (i, message) in failures {
            Self::fail(state, rows[i], &step.node, message);
        }
        state.nodes[event.node.index()].push(Rc::new(output));
    }

    async fn nested(
        step: &Step,
        content: &DecisionNodeContent,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
    ) -> Option<Vec<Result<Variable, NodeError>>> {
        let transform = &content.transform_attributes;
        let plain = transform.input_field.is_none()
            && transform.output_path.is_none()
            && !transform.pass_through
            && matches!(transform.execution_mode, TransformExecutionMode::Single);
        if inputs.len() < 2 || iteration.saturating_add(1) >= max_depth || !plain {
            return None;
        }
        let loader = extensions.loader();
        let graph = loader.load(content.key.as_ref()).await.ok()?.into_graph_arc()?;
        let graph = match graph.compiled_cache.is_some() && graph.resolved_schemas.is_some() {
            true => graph,
            false => {
                let mut owned = (*graph).clone();
                owned.compile();
                let _ = owned.resolve_schemas(loader).await;
                Arc::new(owned)
            }
        };
        let compiled = graph.compiled_plan.as_deref()?.usable(&graph)?;
        let extensions = NodeHandlerExtensions {
            function_runtime: Default::default(),
            compiled_cache: graph.compiled_cache.clone(),
            dt_indexes: graph.dt_indexes.clone(),
            validator_cache: Arc::new(std::cell::OnceCell::from(graph.validator_cache.clone())),
            ..extensions.clone()
        };
        let parents: Vec<Option<Variable>> = (0..inputs.len()).map(|row| nodes.get(row).cloned().flatten()).collect();
        let nesting = Nesting {
            parents: Some(&parents),
            iteration: iteration + 1,
        };
        let state = Box::pin(compiled.run(&graph, &extensions, max_depth, &Source::Rows(inputs), inputs.len(), false, nesting)).await;
        Some(
            state
                .errors
                .into_iter()
                .zip(state.results)
                .map(|(error, result)| match error {
                    Some(error) => Err(NodeError {
                        node_id: step.node.id.clone(),
                        trace: None,
                        source: error.to_string().into(),
                    }),
                    None => Ok(result.unwrap_or_else(Variable::empty_object)),
                })
                .collect(),
        )
    }

    async fn hosted(
        step: &Step,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
        batched: bool,
    ) -> Vec<Result<Variable, NodeError>> {
        if let DecisionNodeKind::DecisionNode { content } = &step.node.kind {
            if let Some(results) = Self::nested(step, content, inputs, nodes, extensions, max_depth, iteration).await {
                return results;
            }
        }
        if let DecisionNodeKind::FunctionNode { content: FunctionNodeContent::Version2(content) } = &step.node.kind {
            let config = NodeContextConfig {
                max_depth,
                trace: false,
                ..Default::default()
            };
            let inputs = inputs.iter().enumerate().map(|(i, value)| (value.clone(), nodes.get(i).cloned().flatten())).collect();
            return FunctionV2NodeHandler::batch(&step.node.id, content, extensions, &config, iteration, inputs).await;
        }
        let mut results = Vec::with_capacity(inputs.len());
        for (i, value) in inputs.iter().enumerate() {
            let nodes = nodes.get(i).cloned().flatten();
            results.push(Self::host(step, value.clone(), nodes, extensions, max_depth, iteration, batched).await);
        }
        results
    }

    async fn bounded(
        &self,
        step: &Step,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
    ) -> (Vec<Variable>, Vec<(usize, String)>) {
        let validator = Self::validator(step, content, extensions);
        let mut values = Vec::with_capacity(inputs.len());
        let mut failures = Vec::new();
        for (i, value) in inputs.iter().enumerate() {
            let nodes = nodes.get(i).cloned().flatten();
            if let Some((validator, schema)) = &validator {
                if validator.is_valid(VariableNode::new(value, &Guards::default())) {
                    values.push(match step.kind {
                        Kind::Input => DeclaredDates::prepare(value, Some(schema)).unwrap_or_else(|| value.clone()),
                        _ => value.clone(),
                    });
                    continue;
                }
            }
            match self.boundary(step, value.clone(), nodes, content, extensions).await {
                Ok(v) => values.push(v),
                Err(e) => {
                    failures.push((i, e));
                    values.push(Variable::Null);
                }
            }
        }
        (values, failures)
    }

    fn rowwise<'a>(&self, runner: &mut LaneRunner, step: &Step, data: Data<'a>, nodes: &[Option<Variable>]) -> (Data<'a>, Vec<(usize, String)>) {
        let rows: Rc<[usize]> = (0..data.len()).collect();
        let based = match &step.kind {
            Kind::Node { transform, .. } if !transform.looped => transform.base.as_deref(),
            _ => None,
        };
        let foreign = matches!(&step.kind, Kind::Node { body: Body::Expression { walked: true, .. }, .. })
            || matches!(&data.shape, data::Shape::Plain(values) if values.iter().any(Self::foreign))
            || Self::foreign_base(&data, based);
        if foreign {
            let mut failures = Vec::new();
            let values: Col = data
                .materialized()
                .iter()
                .enumerate()
                .map(|(i, value)| match Self::walked(step, value.clone(), nodes.get(i).cloned().flatten(), true) {
                    Ok(output) => output,
                    Err(message) => {
                        failures.push((i, message));
                        Variable::Null
                    }
                })
                .collect();
            return (Data::plain(rows, values), failures);
        }
        self.node_with(runner, step, &rows, &Source::Rows(&[]), &data, nodes, &[])
    }

    fn node<'a>(
        &self,
        step: &Step,
        rows: &Rc<[usize]>,
        source: &Source<'a>,
        data: &Data<'a>,
        nodes: &[Option<Variable>],
        extra: &[NodeColumn<'a>],
    ) -> (Data<'a>, Vec<(usize, String)>) {
        Self::RUNNER.with_borrow_mut(|runner| self.node_with(runner, step, rows, source, data, nodes, extra))
    }

    #[allow(clippy::too_many_arguments)]
    fn node_with<'a>(
        &self,
        runner: &mut LaneRunner,
        step: &Step,
        rows: &Rc<[usize]>,
        source: &Source<'a>,
        data: &Data<'a>,
        nodes: &[Option<Variable>],
        extra: &[NodeColumn<'a>],
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let Kind::Node { body, transform } = &step.kind else {
            return (Data::plain(rows.clone(), Rc::from(Vec::new())), Vec::new());
        };
        {
            let wide = rows.len() > 1 || matches!(source, Source::Columns(_));
            let rebase = match (&transform.input_field, &transform.base, transform.looped) {
                (None, _, false) => Some(None),
                (Some(_), Some(base), false) => Some(Some(base.as_ref())),
                _ => None,
            };
            let keyed = !matches!(body, Body::Expression { keys, .. } if keys.is_empty());
            let expression = matches!(body, Body::Expression { .. });
            let overlapping = matches!(body, Body::Expression { overlapping: true, .. });
            match (rebase, &transform.output_path, wide) {
                (Some(rebase), None, true) if !overlapping => {
                    Self::bound(step, data, nodes, extra, rebase, |bound| {
                        Self::layered(runner, body, transform.pass_through, data, bound, None)
                    })
                }
                (Some(rebase), Some(path), true) if keyed && expression && !overlapping => {
                    Self::bound(step, data, nodes, extra, rebase, |bound| {
                        Self::layered(runner, body, transform.pass_through, data, bound, Some(path))
                    })
                }
                (Some(None), Some(path), true) if keyed => {
                    Self::bound(step, data, nodes, extra, None, |bound| {
                        Self::placed(runner, body, path, transform.pass_through, data, bound)
                    })
                }
                (None, Some(path), true)
                    if transform.looped
                        && !step.nodes
                        && transform.input_field.as_ref().is_some_and(|(program, _)| program.path().is_some()) =>
                {
                    Self::looped(runner, step, body, transform, path, data)
                }
                _ => {
                    let materialized = data.materialized();
                    let results = Self::transformed(runner, step, body, transform, &materialized, nodes);
                    let mut failures = Vec::new();
                    let values: Col = results
                        .into_iter()
                        .enumerate()
                        .map(|(i, r)| match r {
                            Ok(v) => v,
                            Err(e) => {
                                failures.push((i, e));
                                Variable::Null
                            }
                        })
                        .collect();
                    (Data::plain(rows.clone(), values), failures)
                }
            }
        }
    }

    fn foreign_base(data: &Data, rebase: Option<&str>) -> bool {
        let Some(base) = rebase else {
            return false;
        };
        let column = data.column_at(base);
        (0..data.len()).any(|row| Self::foreign(&column.get(row)))
    }

    fn immediate(step: &Step) -> bool {
        match &step.kind {
            Kind::Switch { .. } | Kind::Node { .. } => true,
            Kind::Output => !step.schema,
            Kind::Input | Kind::Host => false,
        }
    }

    fn execute_now<'a>(&self, step: &Step, event: &Execute, rows: &Rc<[usize]>, source: &Source<'a>, state: &mut State<'a>, with_nodes: bool) {
        let data = Self::input_of(state, &event.parents, rows);
        let based = match &step.kind {
            Kind::Node { transform, .. } if !transform.looped => transform.base.as_deref(),
            _ => None,
        };
        let foreign = matches!(&step.kind, Kind::Node { body: Body::Expression { walked: true, .. }, .. })
            || matches!(&data.shape, data::Shape::Plain(values) if values.iter().any(Self::foreign))
            || Self::foreign_base(&data, based);
        let (output, failures) = match &step.kind {
            Kind::Node { .. } if foreign => {
                let values = data.materialized();
                let nodes: Vec<Option<Variable>> = match step.nodes && with_nodes {
                    true => self.nodes_of(state, &event.visible, rows).into_iter().map(Some).collect(),
                    false => Vec::new(),
                };
                let mut failures = Vec::new();
                let outputs: Vec<Variable> = values
                    .iter()
                    .enumerate()
                    .map(|(i, value)| match Self::walked(step, value.clone(), nodes.get(i).cloned().flatten(), true) {
                        Ok(output) => output,
                        Err(message) => {
                            failures.push((i, message));
                            Variable::Null
                        }
                    })
                    .collect();
                (Data::plain(rows.clone(), outputs.into()), failures)
            }
            Kind::Node { body, transform } => {
                let column_path = (transform.input_field.is_none() || transform.base.is_some())
                    && !transform.looped
                    && (rows.len() > 1 || matches!(source, Source::Columns(_)))
                    && !(transform.output_path.is_some() && matches!(body, Body::Expression { keys, .. } if keys.is_empty()));
                let (nodes, extra) = self.node_inputs(step, state, &event.visible, rows, with_nodes, column_path);
                self.node(step, rows, source, &data, &nodes, &extra)
            }
            _ => (data, Vec::new()),
        };
        for (i, message) in failures {
            Self::fail(state, rows[i], &step.node, message);
        }
        state.nodes[event.node.index()].push(Rc::new(output));
    }

    async fn validate_input(
        &self,
        step: &Step,
        data: &Data<'_>,
        nodes: &[Option<Variable>],
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
    ) -> Vec<(usize, String)> {
        let mut failures = Vec::new();
        let Some((validator, schema)) = Self::validator(step, content, extensions) else {
            return failures;
        };
        let sure = schema::ObjectSchema::cached(&schema).and_then(|columns| columns.sure(data));
        let doubtful: Vec<(usize, Variable)> = match &sure {
            Some(sure) => (0..data.len())
                .filter(|&i| !sure[i])
                .map(|i| (i, data.materialize_row(i)))
                .collect(),
            None => data.materialized().iter().cloned().enumerate().collect(),
        };
        for (i, value) in doubtful {
            if validator.is_valid(VariableNode::new(&value, &Guards::default())) {
                continue;
            }
            let nodes = nodes.get(i).cloned().flatten();
            if let Err(e) = self.boundary(step, value, nodes, content, extensions).await {
                failures.push((i, e));
            }
        }
        failures
    }

    fn placed<'a>(
        runner: &mut LaneRunner,
        body: &Body,
        path: &Arc<str>,
        pass_through: bool,
        input: &Data<'a>,
        bound: &Bound,
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let count = input.rows.len();
        let mut failures = Vec::new();
        let values: Vec<Variable> = match body {
            Body::Expression { program, entries, keys, .. } => {
                let mut values = vec![Variable::Null; count];
                bound.many(runner, program, None, |row, result| match result {
                    Ok(outputs) => {
                        let object = Variable::empty_object();
                        for (key, value) in keys.iter().zip(outputs) {
                            object.dot_insert(key, value);
                        }
                        values[row] = object;
                    }
                    Err((stage, _)) => failures.push((
                        row,
                        format!(
                            r#"Failed to evaluate expression: "{}""#,
                            entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default()
                        ),
                    )),
                });
                values
            }
            Body::Table(table) => table
                .evaluate(runner, bound)
                .into_iter()
                .map(|out| table.value(out))
                .collect(),
        };
        let layer = Layer::new(Arc::from([path.clone()]), vec![Leaf::Any(values.into())], None);
        let data = match pass_through {
            true => input.layered(layer),
            false => Data::record(&input.rows, layer, None),
        };
        (data, failures)
    }

    fn layered<'a>(
        runner: &mut LaneRunner,
        body: &Body,
        pass_through: bool,
        input: &Data<'a>,
        bound: &Bound,
        prefix: Option<&Arc<str>>,
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let rows = input.rows.clone();
        let count = rows.len();
        let mut failures = Vec::new();
        match body {
            Body::Expression { program, entries, keys, .. } => {
                let mut outs = Vec::new();
                bound.export(runner, program, None, &mut outs, |row, stage, _| {
                    failures.push((
                        row,
                        format!(
                            r#"Failed to evaluate expression: "{}""#,
                            entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default()
                        ),
                    ))
                });
                let keys = match prefix {
                    None => keys.clone(),
                    Some(prefix) => keys.iter().map(|key| Arc::from(format!("{prefix}.{key}"))).collect(),
                };
                let layer = Layer::new(
                    keys,
                    outs.iter_mut().map(|out| Leaf::typed(Array::from_output(out))).collect(),
                    None,
                );
                let data = match pass_through {
                    true => input.layered(layer),
                    false => Data::record(&rows, layer, None),
                };
                (data, failures)
            }
            Body::Table(table) if table.first_hit() => {
                let (columns, present, unmatched) = table.columns(table.first(runner, bound));
                let layer = Layer::new(table.paths().clone(), columns, Some(present));
                let data = match pass_through {
                    true => input.layered(layer),
                    false => Data::record(&rows, layer, Some(unmatched.into())),
                };
                (data, failures)
            }
            Body::Table(table) if table.collects() => {
                let collected = table.first_collect(runner, bound);
                let layer = Layer::new(collected.paths, collected.columns, Some(collected.present));
                let data = match pass_through {
                    true => input.layered(layer),
                    false => Data::record(&rows, layer, Some(collected.unmatched.into())),
                };
                (data, failures)
            }
            Body::Table(table) => {
                let outs = table.evaluate(runner, bound);
                let first = outs.iter().all(|o| matches!(o, TableOut::Leaves(..) | TableOut::Value(Variable::Null)));
                match (pass_through, first) {
                    (true, true) => {
                        let paths = table.paths();
                        let mut columns: Vec<Vec<Variable>> = (0..paths.len()).map(|_| vec![Variable::Null; count]).collect();
                        let mut present: Vec<Vec<bool>> = (0..paths.len()).map(|_| vec![false; count]).collect();
                        for (row, out) in outs.into_iter().enumerate() {
                            if let TableOut::Leaves(slots, values) = out {
                                for (&slot, value) in slots.iter().zip(values) {
                                    columns[slot][row] = value;
                                    present[slot][row] = true;
                                }
                            }
                        }
                        let layer = Layer::new(
                            paths.clone(),
                            columns.into_iter().map(|c| Leaf::Any(Col::from(c))).collect(),
                            Some(
                                present
                                    .into_iter()
                                    .map(|p| Mask::Bits(typed::Bits::of(count, |row| p[row]).into()))
                                    .collect(),
                            ),
                        );
                        (input.layered(layer), failures)
                    }
                    (true, false) => {
                        let materialized = input.materialized();
                        let values: Col = outs
                            .into_iter()
                            .enumerate()
                            .map(|(row, out)| match table.value(out) {
                                output @ Variable::Object(_) => materialized[row].clone().merge_clone(&output),
                                output @ Variable::Array(_) => output,
                                _ => materialized[row].clone(),
                            })
                            .collect();
                        (Data::plain(rows, values), failures)
                    }
                    (false, _) => (
                        Data::plain(rows, outs.into_iter().map(|o| table.value(o)).collect()),
                        failures,
                    ),
                }
            }
        }
    }

    async fn host(
        step: &Step,
        input: Variable,
        nodes: Option<Variable>,
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
        batched: bool,
    ) -> Result<Variable, NodeError> {
        let node = &step.node;
        let extensions = match &node.kind {
            DecisionNodeKind::DecisionNode { .. } if batched => NodeHandlerExtensions {
                function_runtime: Default::default(),
                ..extensions.clone()
            },
            _ => extensions.clone(),
        };
        let base = NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            nodes,
            extensions,
            iteration,
            trace: None,
            config: NodeContextConfig {
                max_depth,
                trace: false,
                ..Default::default()
            },
        };
        let response = match &node.kind {
            DecisionNodeKind::FunctionNode { content } => {
                DecisionGraph::handle(base, content.clone(), FunctionNodeHandler).await
            }
            DecisionNodeKind::DecisionNode { content } => {
                DecisionGraph::handle(base, content.clone(), DecisionNodeHandler::default()).await
            }
            DecisionNodeKind::CustomNode { content } => {
                DecisionGraph::handle(base, content.clone(), CustomNodeHandler).await
            }
            _ => return Ok(base.input),
        };
        response.map(|r| r.output)
    }

    fn validator(
        step: &Step,
        content: &GraphContent,
        extensions: &NodeHandlerExtensions,
    ) -> Option<(Arc<Validator<VariableJson>>, Arc<serde_json::Value>)> {
        let node = &step.node;
        let (schema, salt) = match content.resolved_schemas.as_ref().and_then(|r| r.get(&node.id)) {
            Some((schema, salt)) => (schema.clone(), *salt),
            None => match &node.kind {
                DecisionNodeKind::InputNode { content } => (content.schema.clone()?, 0),
                DecisionNodeKind::OutputNode { content } => (content.schema.clone()?, 0),
                _ => return None,
            },
        };
        let key = ValidatorCache::key(&node.id, &node.name, salt);
        let validator = extensions.validator_cache().get_or_insert(key, &schema).ok()?;
        Some((validator, schema))
    }

    fn columnar_input(step: &Step, content: &GraphContent, extensions: &NodeHandlerExtensions) -> bool {
        match Self::validator(step, content, extensions) {
            None => !step.schema && Self::unresolved(content, &step.node),
            Some((_, schema)) => !DeclaredDates::mentions(&schema),
        }
    }

    fn unresolved(content: &GraphContent, node: &DecisionNode) -> bool {
        content
            .resolved_schemas
            .as_ref()
            .is_none_or(|resolved| !resolved.contains_key(&node.id))
    }

    async fn boundary(
        &self,
        step: &Step,
        input: Variable,
        nodes: Option<Variable>,
        content: &Arc<GraphContent>,
        extensions: &NodeHandlerExtensions,
    ) -> Result<Variable, String> {
        let node = &step.node;
        let mut base = NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            nodes,
            extensions: extensions.clone(),
            iteration: 0,
            trace: None,
            config: NodeContextConfig::default(),
        };
        let resolved = content
            .resolved_schemas
            .as_ref()
            .and_then(|r| r.get(&node.id).cloned());
        if let Some((_, salt)) = &resolved {
            base.config.validation_salt = *salt;
        }
        let response = match &node.kind {
            DecisionNodeKind::InputNode { content } => {
                let content = match resolved {
                    Some((schema, _)) => InputNodeContent { schema: Some(schema) },
                    None => content.clone(),
                };
                DecisionGraph::handle(base, content, InputNodeHandler).await
            }
            DecisionNodeKind::OutputNode { content } => {
                let content = match resolved {
                    Some((schema, _)) => OutputNodeContent { schema: Some(schema) },
                    None => content.clone(),
                };
                DecisionGraph::handle(base, content, OutputNodeHandler).await
            }
            _ => return Ok(base.input),
        };
        response.map(|r| r.output).map_err(|e| e.source.to_string())
    }

    fn transformed(
        runner: &mut LaneRunner,
        step: &Step,
        body: &Body,
        transform: &Transform,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
    ) -> Vec<Result<Variable, String>> {
        let effective: Vec<Result<Variable, String>> = match &transform.input_field {
            None => inputs.iter().cloned().map(Ok).collect(),
            Some((program, source)) => {
                let scopes: Vec<Scope> = inputs
                    .iter()
                    .enumerate()
                    .map(|(row, i)| Self::scope(i, nodes.get(row).and_then(Option::as_ref)))
                    .collect();
                runner
                    .evaluate(program, &scopes)
                    .into_iter()
                    .enumerate()
                    .map(|(row, r)| {
                        r.map_err(|_| Self::isolate_error(&inputs[row], nodes.get(row).and_then(Option::as_ref), source))
                    })
                    .collect()
            }
        };

        let mut outputs: Vec<Result<Variable, String>> = match transform.looped {
            false => {
                let rows: Vec<usize> = (0..inputs.len()).filter(|&r| effective[r].is_ok()).collect();
                let body_inputs: Vec<Variable> = rows
                    .iter()
                    .filter_map(|&r| effective[r].as_ref().ok().cloned())
                    .collect();
                let body_nodes: Vec<Option<Variable>> = rows.iter().map(|&r| nodes.get(r).cloned().flatten()).collect();
                let mut produced = Self::items(runner, step, body, &body_inputs, &body_nodes).into_iter();
                effective
                    .into_iter()
                    .map(|e| e.and_then(|_| produced.next().unwrap_or_else(|| Ok(Variable::Null))))
                    .collect()
            }
            true => {
                let mut owners: Vec<usize> = Vec::new();
                let mut items: Vec<Variable> = Vec::new();
                let mut item_nodes: Vec<Option<Variable>> = Vec::new();
                let mut shaped: Vec<Result<usize, String>> = Vec::with_capacity(inputs.len());
                for (row, value) in effective.iter().enumerate() {
                    match value {
                        Err(e) => shaped.push(Err(e.clone())),
                        Ok(value) => match value.as_array() {
                            None => shaped.push(Err("Expected an array".to_string())),
                            Some(array) => {
                                let array = array.borrow();
                                shaped.push(Ok(array.len()));
                                for item in array.iter() {
                                    owners.push(row);
                                    items.push(item.clone());
                                    item_nodes.push(nodes.get(row).cloned().flatten());
                                }
                            }
                        },
                    }
                }
                let mut produced = Self::items(runner, step, body, &items, &item_nodes).into_iter();
                let mut item_inputs = items.into_iter();
                shaped
                    .into_iter()
                    .map(|shape| {
                        let count = shape?;
                        let mut collected = Vec::with_capacity(count);
                        let mut failure: Option<String> = None;
                        for _ in 0..count {
                            let output = produced.next().unwrap_or_else(|| Ok(Variable::Null));
                            let item = item_inputs.next().unwrap_or(Variable::Null);
                            match (output, failure.is_some()) {
                                (_, true) => {}
                                (Err(e), false) => failure = Some(e),
                                (Ok(out), false) => collected.push(match transform.pass_through {
                                    true => item.clone().merge_clone(&out),
                                    false => out,
                                }),
                            }
                        }
                        match failure {
                            Some(e) => Err(e),
                            None => Ok(Variable::from_array(collected)),
                        }
                    })
                    .collect()
            }
        };

        for (row, output) in outputs.iter_mut().enumerate() {
            let Ok(value) = output else {
                continue;
            };
            if let Some(path) = &transform.output_path {
                let wrapped = Variable::empty_object();
                wrapped.dot_insert(path, value.clone());
                *value = wrapped;
            }
            if transform.pass_through {
                let mut node_input = inputs[row].clone();
                *value = node_input.merge_clone(value);
            }
        }
        outputs
    }

    fn isolate_error(input: &Variable, nodes: Option<&Variable>, source: &str) -> String {
        let mut isolate = Isolate::with_environment(input.clone());
        if let Some(nodes) = nodes {
            isolate.set_local(Variable::nodes_key(), nodes.clone());
        }
        match isolate.run_standard(source) {
            Err(e) => format!("Failed to evaluate expression: {e}"),
            Ok(_) => "Failed to evaluate expression".to_string(),
        }
    }

    fn looped<'a>(
        runner: &mut LaneRunner,
        step: &Step,
        body: &Body,
        transform: &Transform,
        path: &Arc<str>,
        data: &Data<'a>,
    ) -> (Data<'a>, Vec<(usize, String)>) {
        let source = transform
            .input_field
            .as_ref()
            .and_then(|(program, _)| program.path())
            .unwrap_or_default();
        let lists = data.subtree(source);
        let mut items: Vec<Variable> = Vec::new();
        let mut spans: Vec<Option<(usize, usize)>> = Vec::with_capacity(lists.len());
        let mut failures = Vec::new();
        for (row, list) in lists.iter().enumerate() {
            match list.as_array() {
                Some(array) => {
                    let start = items.len();
                    items.extend(array.borrow().iter().cloned());
                    spans.push(Some((start, items.len())));
                }
                None => {
                    failures.push((row, "Expected an array".to_string()));
                    spans.push(None);
                }
            }
        }
        let produced = Self::items(runner, step, body, &items, &[]);
        let mut values = Vec::with_capacity(spans.len());
        for (row, span) in spans.into_iter().enumerate() {
            let Some((start, end)) = span else {
                values.push(Variable::Null);
                continue;
            };
            let mut collected = Vec::with_capacity(end - start);
            let mut failure = None;
            for (item, output) in items[start..end].iter().zip(&produced[start..end]) {
                match output {
                    Ok(out) => collected.push(match transform.pass_through {
                        true => item.clone().merge_clone(out),
                        false => out.clone(),
                    }),
                    Err(e) => {
                        failure = Some(e.clone());
                        break;
                    }
                }
            }
            match failure {
                Some(e) => {
                    failures.push((row, e));
                    values.push(Variable::Null);
                }
                None => values.push(Variable::from_array(collected)),
            }
        }
        let layer = Layer::new(Arc::from([path.clone()]), vec![Leaf::Any(values.into())], None);
        let output = match transform.pass_through {
            true => data.layered(layer),
            false => Data::record(&data.rows, layer, None),
        };
        (output, failures)
    }

    fn items(
        runner: &mut LaneRunner,
        step: &Step,
        body: &Body,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
    ) -> Vec<Result<Variable, String>> {
        if inputs.iter().any(Self::foreign) {
            let kept: Vec<usize> = (0..inputs.len()).filter(|&i| !Self::foreign(&inputs[i])).collect();
            let kept_inputs: Vec<Variable> = kept.iter().map(|&i| inputs[i].clone()).collect();
            let kept_nodes: Vec<Option<Variable>> = kept.iter().map(|&i| nodes.get(i).cloned().flatten()).collect();
            let mut produced = Self::items(runner, step, body, &kept_inputs, &kept_nodes).into_iter();
            return inputs
                .iter()
                .enumerate()
                .map(|(i, input)| match Self::foreign(input) {
                    true => Self::walked(step, input.clone(), nodes.get(i).cloned().flatten(), false),
                    false => produced.next().unwrap_or(Ok(Variable::Null)),
                })
                .collect();
        }
        match Self::item_data(step, body, inputs, nodes) {
            Some(data) => {
                let (output, failures) = Self::item_run(runner, step, body, &data);
                let values = output.materialized();
                let mut results: Vec<Result<Variable, String>> = values.iter().cloned().map(Ok).collect();
                for (item, message) in failures.into_iter().rev() {
                    if let Some(result) = results.get_mut(item) {
                        *result = Err(message);
                    }
                }
                results
            }
            None => Self::body(runner, body, inputs, nodes),
        }
    }

    fn foreign(value: &Variable) -> bool {
        !matches!(value, Variable::Null | Variable::Object(_))
    }

    fn walked(step: &Step, input: Variable, nodes: Option<Variable>, transforms: bool) -> Result<Variable, String> {
        let node = &step.node;
        let base = NodeContextBase {
            id: node.id.clone(),
            name: node.name.clone(),
            input,
            nodes,
            extensions: NodeHandlerExtensions::default(),
            iteration: 0,
            trace: None,
            config: NodeContextConfig {
                trace: false,
                ..Default::default()
            },
        };
        let mut future: std::pin::Pin<Box<dyn std::future::Future<Output = Result<crate::nodes::NodeResponse, NodeError>>>> =
            match &node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    let mut content = content.clone();
                    if !transforms {
                        content.transform_attributes = Default::default();
                    }
                    Box::pin(DecisionGraph::handle(base, content, ExpressionNodeHandler))
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    let mut content = content.clone();
                    if !transforms {
                        content.transform_attributes = Default::default();
                    }
                    Box::pin(DecisionGraph::handle(base, content, DecisionTableNodeHandler))
                }
                _ => return Ok(Variable::Null),
            };
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(result) => result.map(|r| r.output).map_err(|e| e.source.to_string()),
            std::task::Poll::Pending => Err("node did not complete".to_string()),
        }
    }

    fn item_keys(step: &Step, body: &Body) -> Option<Vec<Arc<str>>> {
        if step.root || step.dynamic || matches!(&step.kind, Kind::Node { transform, .. } if transform.base.is_some()) {
            return None;
        }
        let programs: Vec<&LaneProgram> = match body {
            Body::Expression { program, .. } => vec![program],
            Body::Table(table) => table.programs(),
        };
        let mut keys: Vec<Arc<str>> = Vec::new();
        for program in &programs {
            let unary = matches!(program.kind(), zen_expression::ExpressionKind::Unary);
            for key in program.program().site_keys.iter() {
                let key = key.as_deref()?;
                match key.starts_with('$') {
                    true if unary && (key == "$" || key.starts_with("$.")) => {}
                    true => return None,
                    false if !keys.iter().any(|k| k.as_ref() == key) => keys.push(Arc::from(key)),
                    false => {}
                }
            }
        }
        let overlapping = keys.iter().any(|a| {
            keys.iter()
                .any(|b| b.strip_prefix(a.as_ref()).is_some_and(|rest| rest.starts_with('.')))
        });
        (!overlapping).then_some(keys)
    }

    fn item_run<'a>(runner: &mut LaneRunner, step: &Step, body: &Body, data: &Data<'a>) -> (Data<'a>, Vec<(usize, String)>) {
        Self::bound(step, data, &[], &[], None, |bound| Self::layered(runner, body, false, data, bound, None))
    }

    fn item_data<'a>(step: &Step, body: &Body, inputs: &[Variable], nodes: &[Option<Variable>]) -> Option<Data<'a>> {
        if inputs.len() < 2 || nodes.iter().any(Option::is_some) {
            return None;
        }
        let keys = Self::item_keys(step, body)?;
        let mut leaves = Vec::with_capacity(keys.len());
        let mut present = Vec::with_capacity(keys.len());
        for key in &keys {
            let mut builder = ColumnBuilder::with_capacity(inputs.len());
            let mut mask = Vec::with_capacity(inputs.len());
            for item in inputs {
                let value = Data::lookup(item, key);
                mask.push(value.is_some());
                builder.push_variable(value.unwrap_or(Variable::Null));
            }
            leaves.push(Leaf::typed(builder.finish()));
            present.push(Mask::Bits(typed::Bits::of(mask.len(), |row| mask[row]).into()));
        }
        let rows: Rc<[usize]> = (0..inputs.len()).collect();
        Some(Data::record(&rows, Layer::new(keys.into(), leaves, Some(present)), None))
    }

    fn body(
        runner: &mut LaneRunner,
        body: &Body,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
    ) -> Vec<Result<Variable, String>> {
        let scopes: Vec<Scope> = inputs
            .iter()
            .enumerate()
            .map(|(row, i)| Self::scope(i, nodes.get(row).and_then(Option::as_ref)))
            .collect();
        match body {
            Body::Expression { program, entries, .. } => {
                let mut out: Vec<Result<Variable, String>> = vec![Ok(Variable::Null); scopes.len()];
                runner.evaluate_many(program, &scopes, None, |row, result| {
                    out[row] = match result {
                        Some(Ok(values)) => {
                            let object = Variable::empty_object();
                            for ((key, _), value) in entries.iter().zip(values) {
                                object.dot_insert(key, value);
                            }
                            Ok(object)
                        }
                        Some(Err((stage, _))) => Err(format!(
                            r#"Failed to evaluate expression: "{}""#,
                            entries.get(stage).map(|(_, v)| v.as_ref()).unwrap_or_default()
                        )),
                        None => Ok(Variable::Null),
                    };
                });
                out
            }
            Body::Table(table) => table
                .evaluate(runner, &Bound::plain(scopes))
                .into_iter()
                .map(|o| Ok(table.value(o)))
                .collect(),
        }
    }
}

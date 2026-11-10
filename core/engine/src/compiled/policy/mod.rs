pub(crate) mod artifact;
pub(crate) mod blocks;
mod columnar;
mod entity;
mod host;
mod schedule;
pub(crate) mod site;
mod traced;
pub(crate) mod validator;

pub(crate) use host::Host;

use crate::compiled::core::special::Specialized;
use crate::compiled::leaf::Literal;
use crate::compiled::policy::artifact::{Artifact as EvalArtifact, Iteration};
use crate::compiled::policy::blocks::MatchSelection;
use crate::compiled::policy::site::{HostState, InstanceSlot, Iterated, Pick, Picked};
use crate::compiled::table::{Table, TableIr};
use crate::policy::blocks::Block;
use crate::policy::blocks::{AssertionIr, BlockKind, MatchIr};
use crate::workspace::types::BlockRef;
use crate::workspace::types::{EvaluateRequest, EvaluationError, EvaluationResult};
use schedule::{Blocks, Demand, End, Event, Replay, Segment};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::context::EvaluationContext;
use zen_expression::lane::SourceInfo;
use zen_expression::lane::{LaneProgram, LaneRunner, Programs};
use zen_expression::{Scope, Variable};
use zen_types::path::DotPath;

enum Native {
    Expression {
        key: Arc<str>,
        program: Box<LaneProgram>,
    },
    Assertion {
        ir: Arc<AssertionIr>,
        conditions: Vec<LaneProgram>,
    },
    Match {
        ir: Arc<MatchIr>,
        conditions: Vec<Option<LaneProgram>>,
        values: Vec<Option<LaneProgram>>,
        pick: Option<Box<LaneProgram>>,
        constants: Vec<Option<Literal>>,
    },
}

#[derive(Debug, Clone)]
enum Refused {
    Kind,
    Iterated,
    Compile(Arc<str>),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Kind => f.write_str("block kind"),
            Refused::Iterated => f.write_str("iteration not lane safe"),
            Refused::Compile(why) => write!(f, "compile: {why}"),
        }
    }
}

impl Native {
    fn program(source: &str) -> Result<LaneProgram, Refused> {
        LaneProgram::standard(source).map_err(|e| Refused::Compile(Arc::from(e.to_string())))
    }

    fn optional(source: &str) -> Result<Option<LaneProgram>, Refused> {
        match source.is_empty() {
            true => Ok(None),
            false => Self::program(source).map(Some),
        }
    }

    fn pick(ir: &MatchIr) -> Option<Box<LaneProgram>> {
        let mut source = String::new();
        let mut open = 0usize;
        let mut tail = String::from("null");
        for (index, arm) in ir.arms.iter().enumerate() {
            if arm.condition.is_empty() {
                tail = index.to_string();
                break;
            }
            source.push_str(&format!("(({}) == true) ? {index} : (", arm.condition));
            open += 1;
        }
        source.push_str(&tail);
        source.push_str(&")".repeat(open));
        LaneProgram::standard(&source).ok().map(Box::new)
    }

    fn compile(kind: &BlockKind) -> Result<Self, Refused> {
        match kind {
            BlockKind::Expression(ir) if !ir.key.is_empty() && !ir.value.is_empty() => {
                Self::program(&ir.value).map(|program| Native::Expression {
                    key: ir.key.clone(),
                    program: Box::new(program),
                })
            }
            BlockKind::Assertion(ir) => Ok(Native::Assertion {
                ir: ir.clone(),
                conditions: ir
                    .conditions
                    .iter()
                    .map(|c| Self::program(&c.expression))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            BlockKind::Match(ir) => {
                let values = ir
                    .arms
                    .iter()
                    .map(|arm| Self::optional(&arm.value))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut runner = LaneRunner::new();
                let constants = values
                    .iter()
                    .map(|value| Literal::constant(&mut runner, value.as_ref()?))
                    .collect();
                Ok(Native::Match {
                    ir: ir.clone(),
                    conditions: ir
                        .arms
                        .iter()
                        .map(|arm| Self::optional(&arm.condition))
                        .collect::<Result<Vec<_>, _>>()?,
                    values,
                    pick: Self::pick(ir),
                    constants,
                })
            }
            _ => Err(Refused::Kind),
        }
    }
}

struct BlockPlan {
    native: Result<Native, Refused>,
    iterated: bool,
    table: Option<Table>,
    member: Option<Native>,
    iteration: Option<Iteration>,
}

impl BlockPlan {
    fn native(&self) -> Option<&Native> {
        self.native.as_ref().ok()
    }

    fn refused(&self) -> Option<&Refused> {
        self.native.as_ref().err()
    }
}

struct Write {
    path: Arc<str>,
    value: Variable,
}

struct Capture {
    row: usize,
    iterated: Iterated,
    span: std::ops::Range<usize>,
}

struct Lane {
    owner: usize,
    index: usize,
    root: Variable,
    slot: InstanceSlot,
}

enum Arm {
    Failed,
    Matched(Option<Arc<str>>),
}

pub(crate) struct PolicyPlan {
    blocks: Blocks,
    plans: Vec<BlockPlan>,
    roots: Vec<Arc<str>>,
    root: Arc<Segment>,
    special: std::sync::RwLock<Specialized<(usize, u64), Arc<LaneProgram>>>,
    safe: std::sync::OnceLock<Vec<Iteration>>,
    dated_tables: std::sync::OnceLock<bool>,
    fusions: columnar::Fusions,
    programs: Arc<Programs>,
}

struct Rows<'a> {
    eval: EvaluationContext,
    hosts: Vec<Option<Host<'a>>>,
    picks: Vec<Vec<(usize, Picked)>>,
    failures: Vec<Option<EvaluationError>>,
}

impl Rows<'_> {
    fn ensure(&mut self) {
        let rows = self.failures.len();
        if self.hosts.len() < rows {
            self.hosts.resize_with(rows, || None);
        }
        if self.picks.len() < rows {
            self.picks.resize_with(rows, Vec::new);
        }
    }

    fn alive(&self, rows: &mut Vec<usize>) {
        rows.retain(|&row| self.failures[row].is_none());
    }

    fn take(&mut self, row: usize, block: usize) -> Option<Picked> {
        let picks = &mut self.picks[row];
        let at = picks.iter().position(|(b, _)| *b == block)?;
        Some(picks.swap_remove(at).1)
    }

    fn picked(&self, row: usize, block: usize) -> Option<&Picked> {
        self.picks
            .get(row)?
            .iter()
            .find(|(b, _)| *b == block)
            .map(|(_, p)| p)
    }
}

impl PolicyPlan {
    const SPECIAL: usize = 4096;

    thread_local! {
        static RUNNER: RefCell<LaneRunner> = RefCell::new(LaneRunner::new());
    }

    pub(crate) fn compile(artifact: &EvalArtifact, roots: Vec<Arc<str>>) -> Self {
        let blocks = Blocks::new(artifact);
        let root = Arc::new(
            Replay {
                artifact,
                blocks: &blocks,
                roots: &roots,
            }
            .segment(&[]),
        );
        let plans = blocks
            .rules
            .iter()
            .zip(&blocks.refs)
            .map(|(rule, owner)| {
                let iteration = artifact.iteration(rule);
                let (native, iterated) = match &iteration {
                    None => (Native::compile(&rule.kind), false),
                    Some((_, path, _)) if Self::lane_safe(artifact, owner, rule, path) => {
                        (Native::compile(&rule.kind), true)
                    }
                    Some(_) => (Err(Refused::Iterated), false),
                };
                BlockPlan {
                    native,
                    iterated,
                    table: match &rule.kind {
                        BlockKind::DecisionTable(ir) => Table::compile(TableIr::policy(ir)).ok(),
                        _ => None,
                    },
                    member: iteration
                        .as_ref()
                        .and_then(|_| Native::compile(&rule.kind).ok()),
                    iteration,
                }
            })
            .collect();
        Self {
            blocks,
            plans,
            roots,
            root,
            special: std::sync::RwLock::new(Specialized::new(Self::SPECIAL)),
            safe: std::sync::OnceLock::new(),
            dated_tables: std::sync::OnceLock::new(),
            fusions: columnar::Fusions::default(),
            programs: artifact.programs(),
        }
    }

    fn signature(columns: &zen_expression::lane::Columns) -> u64 {
        use std::hash::{Hash, Hasher};
        use zen_expression::lane::{Column, Dictionary, Values};
        let mut hasher = ahash::AHasher::default();
        let shape = |column: &Column, hasher: &mut ahash::AHasher| {
            std::mem::discriminant(&column.values).hash(hasher);
            column.validity.is_some().hash(hasher);
        };
        for (name, column) in &columns.columns {
            name.hash(&mut hasher);
            shape(column, &mut hasher);
            if let Values::List { child, .. } = column.values {
                let child = match child {
                    Dictionary::Column(child) => *child,
                    other => other.column(),
                };
                shape(&child, &mut hasher);
                if let Values::Struct { fields, .. } = child.values {
                    for (field, column) in fields {
                        field.hash(&mut hasher);
                        shape(column, &mut hasher);
                    }
                }
            }
        }
        hasher.finish()
    }

    fn special(
        &self,
        program: &LaneProgram,
        columns: &zen_expression::lane::Columns,
    ) -> Option<Arc<LaneProgram>> {
        let key = (program.program().id as usize, Self::signature(columns));
        if let Some(found) = self.special.read().ok().and_then(|cache| cache.get(&key)) {
            return Some(found);
        }
        let built = Arc::new(program.specialize_columns(columns).ok()?);
        if let Ok(mut cache) = self.special.write() {
            cache.insert(key, built.clone());
        }
        Some(built)
    }

    fn lane_safe(artifact: &EvalArtifact, owner: &BlockRef, rule: &Block, iter_path: &str) -> bool {
        let Some(reads) = artifact.reads.get(owner) else {
            return false;
        };
        let overlaps =
            |a: &str, b: &str| a == b || DotPath::indexed(a, b) || DotPath::indexed(b, a);
        let writes: Vec<Arc<str>> = rule
            .kind
            .write_sites()
            .into_iter()
            .map(|w| w.path)
            .collect();
        let reads_safe = reads.iter().all(|read| {
            !read.unresolved
                && !overlaps(&read.path, iter_path)
                && writes.iter().all(|w| !overlaps(&read.path, w))
        });
        reads_safe
            && rule
                .kind
                .expressions(&rule.id)
                .iter()
                .all(|e| !SourceInfo::reads_root(&e.source) && !SourceInfo::reads_dollar(&e.source))
    }

    fn child(&self, artifact: &EvalArtifact, segment: &Segment, demand: Demand) -> Arc<Segment> {
        let End::Branch { children, .. } = &segment.end else {
            return Arc::new(Segment::finished());
        };
        children.get(&demand, || {
            let mut path = segment.path.to_vec();
            path.push(demand.clone());
            Arc::new(
                Replay {
                    artifact,
                    blocks: &self.blocks,
                    roots: &self.roots,
                }
                .segment(&path),
            )
        })
    }

    pub(crate) fn evaluate(
        &self,
        artifact: &EvalArtifact,
        requests: &[EvaluateRequest],
        eval: &EvaluationContext,
    ) -> Vec<Result<EvaluationResult, EvaluationError>> {
        let start = Instant::now();
        let count = requests.len();
        let mut failures: Vec<Option<EvaluationError>> = (0..count).map(|_| None).collect();
        let stores: Vec<Option<Variable>> = requests
            .iter()
            .enumerate()
            .map(|(row, request)| match artifact.prepare(request) {
                Ok((store, _)) => Some(store),
                Err(error) => {
                    failures[row] = Some(error);
                    None
                }
            })
            .collect();
        let mut state = Rows {
            eval: eval.clone(),
            hosts: stores
                .iter()
                .zip(requests)
                .enumerate()
                .map(|(row, (store, request))| {
                    store.as_ref().map(|store| {
                        Host::new(
                            artifact,
                            store,
                            &request.policy_path,
                            false,
                            false,
                            &self.programs,
                            eval.at(row),
                        )
                    })
                })
                .collect(),
            picks: (0..count).map(|_| Vec::new()).collect(),
            failures,
        };

        let rows: Vec<usize> = (0..count)
            .filter(|&row| state.hosts[row].is_some())
            .collect();
        let mut stack: Vec<(Arc<Segment>, Vec<usize>)> = vec![(self.root.clone(), rows)];
        while let Some((segment, mut rows)) = stack.pop() {
            for event in &segment.events {
                state.alive(&mut rows);
                if rows.is_empty() {
                    break;
                }
                match *event {
                    Event::Select(block) => self.select(block, &rows, &mut state),
                    Event::Commit(block) => self.commit(block, &rows, &mut state),
                }
            }
            state.alive(&mut rows);
            if rows.is_empty() {
                continue;
            }
            let End::Branch { block, .. } = &segment.end else {
                continue;
            };
            let owner = &self.blocks.refs[*block];
            let mut groups: Vec<(Demand, Vec<usize>)> = Vec::new();
            for &row in &rows {
                let demand: Demand = match (state.hosts[row].as_ref(), state.picked(row, *block)) {
                    (Some(host), Some(picked)) => host.state.demanded(owner, picked).into(),
                    _ => Arc::from([]),
                };
                match groups.iter_mut().find(|(d, _)| *d == demand) {
                    Some((_, members)) => members.push(row),
                    None => groups.push((demand, vec![row])),
                }
            }
            for (demand, members) in groups.into_iter().rev() {
                stack.push((self.child(artifact, &segment, demand), members));
            }
        }

        let duration = start.elapsed();
        let Rows { failures, .. } = state;
        failures
            .into_iter()
            .zip(stores)
            .map(|(failure, store)| match (failure, store) {
                (Some(error), _) => Err(error),
                (None, Some(store)) => Ok(EvaluationResult {
                    output: store,
                    duration,
                    trace: None,
                }),
                (None, None) => Ok(EvaluationResult {
                    output: Variable::Null,
                    duration,
                    trace: None,
                }),
            })
            .collect()
    }

    fn select(&self, block: usize, rows: &[usize], state: &mut Rows) {
        let plan = &self.plans[block];
        match (plan.native(), plan.iterated) {
            (Some(Native::Match { ir, conditions, .. }), false) => {
                let delegated = Self::select_match(block, ir, conditions, rows, state);
                self.select_delegated(block, &delegated, state);
            }
            (Some(Native::Match { ir, conditions, .. }), true) => {
                let delegated = self.select_match_iterated(block, ir, conditions, rows, state);
                self.select_delegated(block, &delegated, state);
            }
            _ => self.select_delegated(block, rows, state),
        }
    }

    fn select_delegated(&self, block: usize, rows: &[usize], state: &mut Rows) {
        let (owner, rule) = (&self.blocks.refs[block], &self.blocks.rules[block]);
        for &row in rows {
            let Some(host) = state.hosts[row].as_mut() else {
                continue;
            };
            match host.select(owner, rule) {
                Ok(picked) => state.picks[row].push((block, picked)),
                Err(error) => state.failures[row] = Some(error),
            }
        }
    }

    fn commit(&self, block: usize, rows: &[usize], state: &mut Rows) {
        let plan = &self.plans[block];
        if let (Some(native), true) = (plan.native(), plan.iterated) {
            let delegated = self.commit_iterated(block, native, rows, state);
            self.delegate(block, &delegated, state);
            return;
        }
        match plan.native() {
            Some(Native::Expression { key, program }) => {
                let delegated = self.expression(key, program, rows, state);
                self.delegate(block, &delegated, state);
            }
            Some(Native::Assertion { ir, conditions }) => {
                let delegated = Self::assertion(ir, conditions, rows, state);
                self.delegate(block, &delegated, state);
            }
            Some(Native::Match { ir, values, .. }) => {
                let delegated = Self::commit_match(block, ir, values, rows, state);
                self.delegate(block, &delegated, state);
            }
            None => self.delegate(block, rows, state),
        }
    }

    fn delegate(&self, block: usize, rows: &[usize], state: &mut Rows) {
        let (owner, rule) = (&self.blocks.refs[block], &self.blocks.rules[block]);
        for &row in rows {
            let picked = state.take(row, block);
            let Some(host) = state.hosts[row].as_mut() else {
                continue;
            };
            let result = match picked {
                Some(picked) => host.commit(owner, rule, picked),
                None => host
                    .select(owner, rule)
                    .and_then(|picked| host.commit(owner, rule, picked)),
            };
            if let Err(error) = result {
                state.failures[row] = Some(error);
            }
        }
    }

    fn scopes(rows: &[usize], state: &Rows) -> (Vec<usize>, Vec<Scope>) {
        rows.iter()
            .filter_map(|&row| {
                let host = state.hosts[row].as_ref()?;
                host.state.clear_dollar();
                Some((row, Scope::new(host.state.env().shallow_clone())))
            })
            .unzip()
    }

    fn drawn(eval: &EvaluationContext, program: &LaneProgram, rows: &[usize]) -> EvaluationContext {
        match program.program().draws {
            true => eval.rows(rows),
            false => eval.clone(),
        }
    }

    fn run(
        eval: &EvaluationContext,
        program: &LaneProgram,
        scopes: &[Scope],
        rows: &[usize],
        sink: impl FnMut(usize, Result<Variable, zen_expression::IsolateError>),
    ) {
        let eval = Self::drawn(eval, program, rows);
        Self::RUNNER.with_borrow_mut(|runner| runner.evaluate_with(program, &eval, scopes, sink));
    }

    fn arms(
        eval: &EvaluationContext,
        ir: &MatchIr,
        conditions: &[Option<LaneProgram>],
        scopes: Vec<Scope>,
        rows: &[usize],
    ) -> Vec<Arm> {
        let mut outcome: Vec<Option<Arm>> = (0..scopes.len()).map(|_| None).collect();
        let mut open: Vec<usize> = (0..scopes.len()).collect();
        let mut scopes = scopes;
        for (arm, condition) in ir.arms.iter().zip(conditions) {
            if open.is_empty() {
                break;
            }
            let Some(program) = condition else {
                for lane in open.drain(..) {
                    outcome[lane] = Some(Arm::Matched(Some(arm.id.clone())));
                }
                break;
            };
            let mut hit = vec![None; open.len()];
            let at: Vec<usize> = open.iter().map(|&lane| rows[lane]).collect();
            Self::run(eval, program, &scopes, &at, |i, result| {
                hit[i] = Some(result.map(|v| v.as_bool().unwrap_or(false)).ok())
            });
            let mut next_open = Vec::with_capacity(open.len());
            let mut next_scopes = Vec::with_capacity(open.len());
            for ((lane, scope), hit) in open.drain(..).zip(scopes.drain(..)).zip(hit) {
                match hit.flatten() {
                    None => outcome[lane] = Some(Arm::Failed),
                    Some(true) => outcome[lane] = Some(Arm::Matched(Some(arm.id.clone()))),
                    Some(false) => {
                        next_open.push(lane);
                        next_scopes.push(scope);
                    }
                }
            }
            open = next_open;
            scopes = next_scopes;
        }
        outcome
            .into_iter()
            .map(|o| o.unwrap_or(Arm::Matched(None)))
            .collect()
    }

    fn selection(matched_arm: Option<Arc<str>>) -> Pick {
        Pick::Match(MatchSelection {
            matched_arm,
            arms: Vec::new(),
        })
    }

    fn select_match(
        block: usize,
        ir: &MatchIr,
        conditions: &[Option<LaneProgram>],
        rows: &[usize],
        state: &mut Rows,
    ) -> Vec<usize> {
        let (live, scopes) = Self::scopes(rows, state);
        let mut delegated = Vec::new();
        let arms = Self::arms(&state.eval, ir, conditions, scopes, &live);
        for (row, arm) in live.into_iter().zip(arms) {
            match arm {
                Arm::Failed => delegated.push(row),
                Arm::Matched(matched_arm) => {
                    state.picks[row].push((block, Picked::Singleton(Self::selection(matched_arm))))
                }
            }
        }
        delegated
    }

    fn lanes(&self, block: usize, rows: &[usize], state: &mut Rows) -> (Vec<Lane>, Vec<Capture>) {
        let rule = &self.blocks.rules[block];
        let mut lanes = Vec::new();
        let mut captured = Vec::new();
        for &row in rows {
            let picked = state.take(row, block);
            let Some(host) = state.hosts[row].as_ref() else {
                continue;
            };
            let iterated = match picked {
                Some(Picked::Iterated(iterated)) => iterated,
                Some(Picked::Skipped) => continue,
                Some(other) => {
                    state.picks[row].push((block, other));
                    continue;
                }
                None => match host.state.capture(rule) {
                    Some(iterated) => iterated,
                    None => continue,
                },
            };
            let (start, owner) = (lanes.len(), captured.len());
            for (index, bound) in host
                .state
                .instance_scopes(&iterated)
                .into_iter()
                .enumerate()
            {
                if let Some((root, slot)) = bound {
                    lanes.push(Lane {
                        owner,
                        index,
                        root,
                        slot,
                    });
                }
            }
            captured.push(Capture {
                row,
                iterated,
                span: start..lanes.len(),
            });
        }
        (lanes, captured)
    }

    fn select_match_iterated(
        &self,
        block: usize,
        ir: &MatchIr,
        conditions: &[Option<LaneProgram>],
        rows: &[usize],
        state: &mut Rows,
    ) -> Vec<usize> {
        let (lanes, captured) = self.lanes(block, rows, state);
        let scopes: Vec<Scope> = lanes
            .iter()
            .map(|l| Scope::new(l.root.shallow_clone()))
            .collect();
        let owners: Vec<usize> = lanes.iter().map(|l| captured[l.owner].row).collect();
        let arms = Self::arms(&state.eval, ir, conditions, scopes, &owners);
        let mut delegated = Vec::new();
        for Capture {
            row,
            iterated,
            span,
        } in captured
        {
            let mut picks: Vec<Pick> = iterated
                .instances()
                .iter()
                .map(|_| Pick::Unconditional)
                .collect();
            let mut failed = false;
            for (lane, arm) in lanes[span.clone()].iter().zip(&arms[span]) {
                match arm {
                    Arm::Failed => failed = true,
                    Arm::Matched(matched_arm) => {
                        picks[lane.index] = Self::selection(matched_arm.clone())
                    }
                }
            }
            match failed {
                true => delegated.push(row),
                false => state.picks[row].push((block, iterated.picked(picks))),
            }
        }
        delegated
    }

    fn commit_iterated(
        &self,
        block: usize,
        native: &Native,
        rows: &[usize],
        state: &mut Rows,
    ) -> Vec<usize> {
        let (owner, rule) = (&self.blocks.refs[block], &self.blocks.rules[block]);
        let (lanes, captured) = self.lanes(block, rows, state);
        let scopes: Vec<Scope> = lanes
            .iter()
            .map(|l| Scope::new(l.root.shallow_clone()))
            .collect();
        let owners: Vec<usize> = lanes.iter().map(|l| captured[l.owner].row).collect();
        let eval = state.eval.clone();
        let mut writes: Vec<Option<Option<Write>>> = (0..lanes.len()).map(|_| None).collect();
        match native {
            Native::Expression { key, program } => {
                Self::run(&eval, program, &scopes, &owners, |i, result| {
                    writes[i] = result.ok().map(|value| {
                        Some(Write {
                            path: key.clone(),
                            value,
                        })
                    })
                })
            }
            Native::Assertion { ir, conditions } => {
                let mut results: Vec<Option<Vec<bool>>> =
                    (0..lanes.len()).map(|_| Some(Vec::new())).collect();
                for program in conditions {
                    Self::run(&eval, program, &scopes, &owners, |i, result| {
                        match (result, &mut results[i]) {
                            (Ok(value), Some(r)) => r.push(value.as_bool().unwrap_or(false)),
                            _ => results[i] = None,
                        }
                    });
                }
                for (i, result) in results.into_iter().enumerate() {
                    writes[i] = result.map(|r| {
                        (!ir.output.is_empty()).then(|| Write {
                            path: ir.output.clone(),
                            value: Variable::Bool(ir.fold(&r)),
                        })
                    });
                }
            }
            Native::Match { ir, values, .. } => {
                let key = (!ir.key.is_empty()).then(|| ir.key.clone());
                let arm_of = |lane: &Lane| match captured
                    .get(lane.owner)?
                    .iterated
                    .picks()
                    .get(lane.index)?
                {
                    Pick::Match(selection) => {
                        let id = selection.matched_arm.as_ref()?;
                        ir.arms.iter().position(|a| &a.id == id)
                    }
                    _ => None,
                };
                let mut groups: Vec<Vec<usize>> = vec![Vec::new(); ir.arms.len()];
                for (i, lane) in lanes.iter().enumerate() {
                    match arm_of(lane).filter(|&arm| values[arm].is_some()) {
                        Some(arm) => groups[arm].push(i),
                        None => {
                            writes[i] = Some(key.clone().map(|path| Write {
                                path,
                                value: Variable::Null,
                            }))
                        }
                    }
                }
                for (arm, members) in groups.into_iter().enumerate() {
                    let Some(program) = values[arm].as_ref().filter(|_| !members.is_empty()) else {
                        continue;
                    };
                    let subset: Vec<Scope> = members.iter().map(|&i| scopes[i].clone()).collect();
                    let at: Vec<usize> = members.iter().map(|&i| owners[i]).collect();
                    Self::run(&eval, program, &subset, &at, |j, result| {
                        writes[members[j]] = result
                            .ok()
                            .map(|value| key.clone().map(|path| Write { path, value }))
                    });
                }
            }
        }
        let mut delegated = Vec::new();
        for Capture {
            row,
            iterated,
            span,
        } in captured
        {
            let failed = writes[span.clone()].iter().any(Option::is_none);
            if failed {
                match iterated.picks().is_empty() {
                    true => delegated.push(row),
                    false => {
                        if let Some(host) = state.hosts[row].as_mut() {
                            let committed = host.commit(owner, rule, Picked::Iterated(iterated));
                            if let Err(error) = committed {
                                state.failures[row] = Some(error);
                            }
                        }
                    }
                }
                continue;
            }
            for (lane, write) in lanes[span.clone()].iter().zip(&writes[span]) {
                if let Some(Some(Write { path, value })) = write {
                    HostState::write_into(&lane.root, None, path, value.clone());
                }
                lane.slot.write_back(&iterated.instances()[lane.index]);
            }
        }
        delegated
    }

    fn commit_match(
        block: usize,
        ir: &MatchIr,
        values: &[Option<LaneProgram>],
        rows: &[usize],
        state: &mut Rows,
    ) -> Vec<usize> {
        let mut delegated = Vec::new();
        let mut by_arm: Vec<Vec<usize>> = vec![Vec::new(); ir.arms.len()];
        let mut nulls: Vec<usize> = Vec::new();
        for &row in rows {
            let arm = match state.picked(row, block) {
                Some(Picked::Singleton(Pick::Match(selection))) => selection
                    .matched_arm
                    .as_ref()
                    .and_then(|id| ir.arms.iter().position(|a| &a.id == id)),
                _ => {
                    delegated.push(row);
                    continue;
                }
            };
            match arm {
                Some(arm) if values[arm].is_some() => by_arm[arm].push(row),
                _ => nulls.push(row),
            }
        }
        for row in nulls {
            state.take(row, block);
            if let (false, Some(host)) = (ir.key.is_empty(), state.hosts[row].as_ref()) {
                host.state.write(&ir.key, Variable::Null);
            }
        }
        for (arm, members) in by_arm.into_iter().enumerate() {
            let Some(program) = values[arm].as_ref().filter(|_| !members.is_empty()) else {
                continue;
            };
            let (live, scopes) = Self::scopes(&members, state);
            let mut results: Vec<Option<Variable>> = vec![None; live.len()];
            Self::run(&state.eval, program, &scopes, &live, |i, result| {
                results[i] = result.ok()
            });
            for (row, result) in live.into_iter().zip(results) {
                match result {
                    Some(value) => {
                        state.take(row, block);
                        if let (false, Some(host)) = (ir.key.is_empty(), state.hosts[row].as_ref())
                        {
                            host.state.write(&ir.key, value);
                        }
                    }
                    None => delegated.push(row),
                }
            }
        }
        delegated
    }

    fn assertion(
        ir: &AssertionIr,
        conditions: &[LaneProgram],
        rows: &[usize],
        state: &mut Rows,
    ) -> Vec<usize> {
        let (live, scopes) = Self::scopes(rows, state);
        let mut results: Vec<Vec<bool>> = vec![Vec::with_capacity(conditions.len()); live.len()];
        let mut failed = vec![false; live.len()];
        for program in conditions {
            Self::run(
                &state.eval,
                program,
                &scopes,
                &live,
                |i, result| match result {
                    Ok(value) => results[i].push(value.as_bool().unwrap_or(false)),
                    Err(_) => failed[i] = true,
                },
            );
        }
        let mut delegated = Vec::new();
        for (i, row) in live.into_iter().enumerate() {
            if failed[i] {
                delegated.push(row);
                continue;
            }
            let outcome = ir.fold(&results[i]);
            if let (false, Some(host)) = (ir.output.is_empty(), state.hosts[row].as_ref()) {
                host.state.write(&ir.output, Variable::Bool(outcome));
            }
        }
        delegated
    }

    fn expression(
        &self,
        key: &Arc<str>,
        program: &LaneProgram,
        rows: &[usize],
        state: &mut Rows,
    ) -> Vec<usize> {
        let (live, scopes) = Self::scopes(rows, state);
        let mut delegated = Vec::new();
        let eval = Self::drawn(&state.eval, program, &live);
        Self::RUNNER.with_borrow_mut(|runner| {
            runner.evaluate_with(program, &eval, &scopes, |i, result| {
                let row = live[i];
                match (result, state.hosts[row].as_ref()) {
                    (Ok(value), Some(host)) => host.state.write(key, value),
                    _ => delegated.push(row),
                }
            })
        });
        delegated
    }
}

mod schedule;

use crate::policy::blocks::{AssertionIr, BlockKind, MatchIr, MatchSelection};
use crate::policy::blocks::{Block, ExecutionContext};
use crate::policy::evaluator::{Driver, EvalArtifact, InstanceSlot, Iterated, Pick, Picked};
use crate::workspace::types::BlockRef;
use zen_expression::lane::SourceInfo;
use crate::workspace::types::{EvaluateRequest, EvaluationError, EvaluationResult};
use schedule::{Blocks, Demand, End, Event, Replay, Segment};
use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;
use zen_expression::lane::{LaneProgram, LaneRunner};
use zen_expression::{Scope, Variable};

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
    },
}

impl Native {
    fn optional(source: &str) -> Option<Option<LaneProgram>> {
        match source.is_empty() {
            true => Some(None),
            false => LaneProgram::standard(source).ok().map(Some),
        }
    }

    fn compile(kind: &BlockKind) -> Option<Self> {
        match kind {
            BlockKind::Expression(ir) if !ir.key.is_empty() && !ir.value.is_empty() => {
                LaneProgram::standard(&ir.value).ok().map(|program| Native::Expression {
                    key: ir.key.clone(),
                    program: Box::new(program),
                })
            }
            BlockKind::Assertion(ir) => Some(Native::Assertion {
                ir: ir.clone(),
                conditions: ir
                    .conditions
                    .iter()
                    .map(|c| LaneProgram::standard(&c.expression).ok())
                    .collect::<Option<Vec<_>>>()?,
            }),
            BlockKind::Match(ir) => Some(Native::Match {
                ir: ir.clone(),
                conditions: ir
                    .arms
                    .iter()
                    .map(|arm| Self::optional(&arm.condition))
                    .collect::<Option<Vec<_>>>()?,
                values: ir
                    .arms
                    .iter()
                    .map(|arm| Self::optional(&arm.value))
                    .collect::<Option<Vec<_>>>()?,
            }),
            _ => None,
        }
    }
}

type Write = Option<(Arc<str>, Variable)>;

struct Lane {
    row: usize,
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
    natives: Vec<Option<Native>>,
    iterated: Vec<bool>,
    roots: Vec<Arc<str>>,
    root: Arc<Segment>,
}

struct Rows<'a> {
    drivers: Vec<Option<Driver<'a>>>,
    picks: Vec<Vec<(usize, Picked)>>,
    failures: Vec<Option<EvaluationError>>,
}

impl Rows<'_> {
    fn alive(&self, rows: &mut Vec<usize>) {
        rows.retain(|&row| self.failures[row].is_none());
    }

    fn take(&mut self, row: usize, block: usize) -> Option<Picked> {
        let picks = &mut self.picks[row];
        let at = picks.iter().position(|(b, _)| *b == block)?;
        Some(picks.swap_remove(at).1)
    }

    fn picked(&self, row: usize, block: usize) -> Option<&Picked> {
        self.picks[row].iter().find(|(b, _)| *b == block).map(|(_, p)| p)
    }
}

impl PolicyPlan {
    thread_local! {
        static RUNNER: RefCell<LaneRunner> = RefCell::new(LaneRunner::new());
    }

    pub(crate) fn compile(artifact: &EvalArtifact, roots: Vec<Arc<str>>) -> Self {
        let blocks = Blocks::new(artifact);
        let (natives, iterated): (Vec<Option<Native>>, Vec<bool>) = blocks
            .rules
            .iter()
            .zip(&blocks.refs)
            .map(|(rule, owner)| match artifact.iteration(rule) {
                None => (Native::compile(&rule.kind), false),
                Some((_, path, _)) if Self::lane_safe(artifact, owner, rule, &path) => {
                    (Native::compile(&rule.kind), true)
                }
                Some(_) => (None, false),
            })
            .unzip();
        let root = Arc::new(
            Replay {
                artifact,
                blocks: &blocks,
                roots: &roots,
            }
            .segment(&[]),
        );
        Self {
            blocks,
            natives,
            iterated,
            roots,
            root,
        }
    }

    fn lane_safe(artifact: &EvalArtifact, owner: &BlockRef, rule: &Block, iter_path: &str) -> bool {
        let Some(reads) = artifact.reads.get(owner) else {
            return false;
        };
        let overlaps = |a: &str, b: &str| {
            a == b
                || a.strip_prefix(b).is_some_and(|r| r.starts_with('.') || r.starts_with('['))
                || b.strip_prefix(a).is_some_and(|r| r.starts_with('.') || r.starts_with('['))
        };
        let writes: Vec<Arc<str>> = rule.kind.write_sites().into_iter().map(|w| w.path).collect();
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
        if let Some(child) = children.read().ok().and_then(|c| c.get(&demand).cloned()) {
            return child;
        }
        let mut path = segment.path.to_vec();
        path.push(demand.clone());
        let child = Arc::new(
            Replay {
                artifact,
                blocks: &self.blocks,
                roots: &self.roots,
            }
            .segment(&path),
        );
        if let Ok(mut c) = children.write() {
            return c.entry(demand).or_insert(child).clone();
        }
        child
    }

    pub(crate) fn evaluate(
        &self,
        artifact: &EvalArtifact,
        requests: &[EvaluateRequest],
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
            drivers: stores
                .iter()
                .zip(requests)
                .map(|(store, request)| {
                    store
                        .as_ref()
                        .map(|store| Driver::new(artifact, store, &request.policy_path, false, false))
                })
                .collect(),
            picks: (0..count).map(|_| Vec::new()).collect(),
            failures,
        };

        let rows: Vec<usize> = (0..count).filter(|&row| state.drivers[row].is_some()).collect();
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
                let demand: Demand = match (state.drivers[row].as_ref(), state.picked(row, *block)) {
                    (Some(driver), Some(picked)) => driver.demanded(owner, picked).into(),
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
        match (&self.natives[block], self.iterated[block]) {
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
            let Some(driver) = state.drivers[row].as_mut() else {
                continue;
            };
            match driver.select(owner, rule) {
                Ok(picked) => state.picks[row].push((block, picked)),
                Err(error) => state.failures[row] = Some(error),
            }
        }
    }

    fn commit(&self, block: usize, rows: &[usize], state: &mut Rows) {
        if let (Some(native), true) = (&self.natives[block], self.iterated[block]) {
            let delegated = self.commit_iterated(block, native, rows, state);
            self.delegate(block, &delegated, state);
            return;
        }
        match &self.natives[block] {
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
            let Some(driver) = state.drivers[row].as_mut() else {
                continue;
            };
            let result = match picked {
                Some(picked) => driver.commit(owner, rule, picked),
                None => driver
                    .select(owner, rule)
                    .and_then(|picked| driver.commit(owner, rule, picked)),
            };
            if let Err(error) = result {
                state.failures[row] = Some(error);
            }
        }
    }

    fn scopes(rows: &[usize], state: &Rows) -> (Vec<usize>, Vec<Scope>) {
        rows.iter()
            .filter_map(|&row| {
                let driver = state.drivers[row].as_ref()?;
                driver.clear_dollar();
                Some((row, Scope::new(driver.env().shallow_clone())))
            })
            .unzip()
    }

    fn run(program: &LaneProgram, scopes: &[Scope], sink: impl FnMut(usize, Result<Variable, zen_expression::IsolateError>)) {
        Self::RUNNER.with_borrow_mut(|runner| runner.evaluate_with(program, scopes, sink));
    }

    fn arms(ir: &MatchIr, conditions: &[Option<LaneProgram>], scopes: Vec<Scope>) -> Vec<Arm> {
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
            Self::run(program, &scopes, |i, result| {
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
        for (row, arm) in live.into_iter().zip(Self::arms(ir, conditions, scopes)) {
            match arm {
                Arm::Failed => delegated.push(row),
                Arm::Matched(matched_arm) => state.picks[row]
                    .push((block, Picked::Singleton(Self::selection(matched_arm)))),
            }
        }
        delegated
    }

    fn lanes(&self, block: usize, rows: &[usize], state: &mut Rows) -> (Vec<Lane>, Vec<(usize, Iterated)>) {
        let rule = &self.blocks.rules[block];
        let mut lanes = Vec::new();
        let mut captured = Vec::new();
        for &row in rows {
            let picked = state.take(row, block);
            let Some(driver) = state.drivers[row].as_ref() else {
                continue;
            };
            let iterated = match picked {
                Some(Picked::Iterated(iterated)) => iterated,
                Some(Picked::Skipped) => continue,
                Some(other) => {
                    state.picks[row].push((block, other));
                    continue;
                }
                None => match driver.capture(rule) {
                    Some(iterated) => iterated,
                    None => continue,
                },
            };
            for (index, bound) in driver.instance_scopes(&iterated).into_iter().enumerate() {
                if let Some((root, slot)) = bound {
                    lanes.push(Lane { row, index, root, slot });
                }
            }
            captured.push((row, iterated));
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
        let scopes: Vec<Scope> = lanes.iter().map(|l| Scope::new(l.root.shallow_clone())).collect();
        let arms = Self::arms(ir, conditions, scopes);
        let mut delegated = Vec::new();
        for (row, iterated) in captured {
            let mut picks: Vec<Pick> = iterated.instances().iter().map(|_| Pick::Unconditional).collect();
            let mut failed = false;
            for (lane, arm) in lanes.iter().zip(&arms).filter(|(l, _)| l.row == row) {
                match arm {
                    Arm::Failed => failed = true,
                    Arm::Matched(matched_arm) => picks[lane.index] = Self::selection(matched_arm.clone()),
                }
            }
            match failed {
                true => delegated.push(row),
                false => state.picks[row].push((block, iterated.picked(picks))),
            }
        }
        delegated
    }

    fn commit_iterated(&self, block: usize, native: &Native, rows: &[usize], state: &mut Rows) -> Vec<usize> {
        let (owner, rule) = (&self.blocks.refs[block], &self.blocks.rules[block]);
        let (lanes, captured) = self.lanes(block, rows, state);
        let scopes: Vec<Scope> = lanes.iter().map(|l| Scope::new(l.root.shallow_clone())).collect();
        let mut writes: Vec<Option<Write>> = (0..lanes.len()).map(|_| None).collect();
        match native {
            Native::Expression { key, program } => Self::run(program, &scopes, |i, result| {
                writes[i] = result.ok().map(|value| Some((key.clone(), value)))
            }),
            Native::Assertion { ir, conditions } => {
                let mut results: Vec<Option<Vec<bool>>> = (0..lanes.len()).map(|_| Some(Vec::new())).collect();
                for program in conditions {
                    Self::run(program, &scopes, |i, result| match (result, &mut results[i]) {
                        (Ok(value), Some(r)) => r.push(value.as_bool().unwrap_or(false)),
                        _ => results[i] = None,
                    });
                }
                for (i, result) in results.into_iter().enumerate() {
                    writes[i] = result.map(|r| {
                        (!ir.output.is_empty()).then(|| (ir.output.clone(), Variable::Bool(ir.fold(&r))))
                    });
                }
            }
            Native::Match { ir, values, .. } => {
                let key = (!ir.key.is_empty()).then(|| ir.key.clone());
                let arm_of = |lane: &Lane| {
                    let (_, iterated) = captured.iter().find(|(r, _)| *r == lane.row)?;
                    match iterated.picks().get(lane.index)? {
                        Pick::Match(selection) => {
                            let id = selection.matched_arm.as_ref()?;
                            ir.arms.iter().position(|a| &a.id == id)
                        }
                        _ => None,
                    }
                };
                let mut groups: Vec<Vec<usize>> = vec![Vec::new(); ir.arms.len()];
                for (i, lane) in lanes.iter().enumerate() {
                    match arm_of(lane).filter(|&arm| values[arm].is_some()) {
                        Some(arm) => groups[arm].push(i),
                        None => writes[i] = Some(key.clone().map(|k| (k, Variable::Null))),
                    }
                }
                for (arm, members) in groups.into_iter().enumerate() {
                    let Some(program) = values[arm].as_ref().filter(|_| !members.is_empty()) else {
                        continue;
                    };
                    let subset: Vec<Scope> = members.iter().map(|&i| scopes[i].clone()).collect();
                    Self::run(program, &subset, |j, result| {
                        writes[members[j]] = result.ok().map(|value| key.clone().map(|k| (k, value)))
                    });
                }
            }
        }
        let mut delegated = Vec::new();
        for (row, iterated) in captured {
            let failed = lanes.iter().zip(&writes).any(|(l, w)| l.row == row && w.is_none());
            if failed {
                match iterated.picks().is_empty() {
                    true => delegated.push(row),
                    false => {
                        if let Some(driver) = state.drivers[row].as_mut() {
                            if let Err(error) = driver.commit(owner, rule, Picked::Iterated(iterated)) {
                                state.failures[row] = Some(error);
                            }
                        }
                    }
                }
                continue;
            }
            for (lane, write) in lanes.iter().zip(&writes).filter(|(l, _)| l.row == row) {
                if let Some(Some((path, value))) = write {
                    ExecutionContext::write_into(&lane.root, None, path, value.clone());
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
            if let (false, Some(driver)) = (ir.key.is_empty(), state.drivers[row].as_ref()) {
                driver.write(&ir.key, Variable::Null);
            }
        }
        for (arm, members) in by_arm.into_iter().enumerate() {
            let Some(program) = values[arm].as_ref().filter(|_| !members.is_empty()) else {
                continue;
            };
            let (live, scopes) = Self::scopes(&members, state);
            let mut results: Vec<Option<Variable>> = vec![None; live.len()];
            Self::run(program, &scopes, |i, result| results[i] = result.ok());
            for (row, result) in live.into_iter().zip(results) {
                match result {
                    Some(value) => {
                        state.take(row, block);
                        if let (false, Some(driver)) = (ir.key.is_empty(), state.drivers[row].as_ref()) {
                            driver.write(&ir.key, value);
                        }
                    }
                    None => delegated.push(row),
                }
            }
        }
        delegated
    }

    fn assertion(ir: &AssertionIr, conditions: &[LaneProgram], rows: &[usize], state: &mut Rows) -> Vec<usize> {
        let (live, scopes) = Self::scopes(rows, state);
        let mut results: Vec<Vec<bool>> = vec![Vec::with_capacity(conditions.len()); live.len()];
        let mut failed = vec![false; live.len()];
        for program in conditions {
            Self::run(program, &scopes, |i, result| match result {
                Ok(value) => results[i].push(value.as_bool().unwrap_or(false)),
                Err(_) => failed[i] = true,
            });
        }
        let mut delegated = Vec::new();
        for (i, row) in live.into_iter().enumerate() {
            if failed[i] {
                delegated.push(row);
                continue;
            }
            let outcome = ir.fold(&results[i]);
            if let (false, Some(driver)) = (ir.output.is_empty(), state.drivers[row].as_ref()) {
                driver.write(&ir.output, Variable::Bool(outcome));
            }
        }
        delegated
    }

    fn expression(&self, key: &Arc<str>, program: &LaneProgram, rows: &[usize], state: &mut Rows) -> Vec<usize> {
        let (live, scopes) = Self::scopes(rows, state);
        let mut delegated = Vec::new();
        Self::RUNNER.with_borrow_mut(|runner| {
            runner.evaluate_with(program, &scopes, |i, result| {
                let row = live[i];
                match (result, state.drivers[row].as_ref()) {
                    (Ok(value), Some(driver)) => driver.write(key, value),
                    _ => delegated.push(row),
                }
            })
        });
        delegated
    }
}

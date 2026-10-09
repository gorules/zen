use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;

use ahash::{HashMap, HashSet, HashSetExt};
use base64::Engine as _;
use fixedbitset::FixedBitSet;
use zen_expression::variable::Variable;
use zen_expression::{Isolate, IsolateError};
use zen_types::decision::DecisionTableInputField;

use crate::policy::blocks::{
    AssertionIr, Block, BlockKind, DecisionTableIr, ExecutionError, ExpressionIr, MatchIr,
    MatchSelection, TableSelection,
};
use crate::policy::evaluator::{EvalArtifact, StoreOps};
use crate::policy::host::{Evaluate, HostState, Pick, Picked, Site};
use crate::workspace::db::Db;
use crate::workspace::types::{
    BlockExecution, BlockRef, BlockTrace, ConditionTrace, DecisionTableExtras, EvaluateRequest,
    EvaluationError, EvaluationResult, Trace, WriteTrace,
};

impl Db {
    pub fn evaluate_with_driver(
        &self,
        req: &EvaluateRequest,
    ) -> Result<EvaluationResult, EvaluationError> {
        self.check_evaluable(req)?;
        self.eval_artifact(&req.policy_path)
            .evaluate_with_driver(req, false)
    }

    pub fn enhance_trace_with_driver(
        &self,
        req: &EvaluateRequest,
    ) -> Result<EvaluationResult, EvaluationError> {
        self.check_evaluable(req)?;
        let mut req = req.clone();
        req.trace = true;
        self.eval_artifact(&req.policy_path)
            .evaluate_with_driver(&req, true)
    }
}

impl EvalArtifact {
    pub(crate) fn evaluate_with_driver(
        &self,
        req: &EvaluateRequest,
        extras: bool,
    ) -> Result<EvaluationResult, EvaluationError> {
        let start = Instant::now();
        let (store, order_to_run) = self.prepare(req)?;
        let roots = self.roots(req);
        let mut driver = Driver::new(self, &store, &req.policy_path, req.trace, extras);
        let outcome = roots.iter().try_for_each(|root| driver.demand(root));

        let trace = req.trace.then(|| Trace {
            engine_version: Arc::from(crate::ENGINE_VERSION),
            properties: store.snapshot(&order_to_run),
            executions: driver.executions(),
        });

        if let Err(error) = outcome {
            return Err(error.with_partial_trace(trace));
        }

        Ok(EvaluationResult {
            output: store,
            duration: start.elapsed(),
            trace,
        })
    }

    pub(crate) fn roots(&self, req: &EvaluateRequest) -> Vec<Arc<str>> {
        match req.goals.is_empty() {
            true => self.eval_graph.terminal_sinks(&self.members),
            false => req.goals.clone(),
        }
    }
}

pub(crate) struct Driver<'a> {
    state: HostState<'a>,
    isolate: RefCell<Isolate>,
    ran: HashSet<BlockRef>,
    in_progress: HashSet<BlockRef>,
}

impl Evaluate for RefCell<Isolate> {
    fn environment(&mut self, env: Variable) {
        self.borrow_mut().set_environment(env);
    }

    fn select(&mut self, rule: &Block, site: &Site) -> Result<Pick, ExecutionError> {
        let ctx = ExecutionContext::of(site, self);
        match &rule.kind {
            BlockKind::Match(m) => m.select(&ctx).map(Pick::Match),
            BlockKind::DecisionTable(d) => d.select(&ctx).map(Pick::Table),
            BlockKind::Expression(_) | BlockKind::Assertion(_) => Ok(Pick::Unconditional),
        }
    }

    fn commit(&mut self, rule: &Block, site: &Site, pick: &Pick) -> Result<BlockTrace, ExecutionError> {
        let ctx = ExecutionContext::of(site, self);
        match (&rule.kind, pick) {
            (BlockKind::Match(m), Pick::Match(selection)) => m.commit(&ctx, selection),
            (BlockKind::DecisionTable(d), Pick::Table(selection)) => d.commit(&ctx, selection),
            _ => rule.execute(&ctx),
        }
    }
}

impl<'a> Driver<'a> {
    pub(crate) fn new(
        artifact: &'a EvalArtifact,
        store: &'a Variable,
        entry: &'a Arc<str>,
        trace: bool,
        extras: bool,
    ) -> Self {
        Self {
            isolate: RefCell::new(Isolate::new().with_cache(Some(artifact.opcode_cache.clone()))),
            state: HostState::new(artifact, store, entry, trace, extras),
            ran: HashSet::new(),
            in_progress: HashSet::new(),
        }
    }

    pub(crate) fn executions(self) -> Vec<BlockExecution> {
        self.state.executions()
    }

    pub(crate) fn demand(&mut self, prop: &str) -> Result<(), EvaluationError> {
        self.state
            .writers_of_longest_prefix(prop)
            .iter()
            .try_for_each(|owner| self.run_block(owner))
    }

    fn run_block(&mut self, owner: &BlockRef) -> Result<(), EvaluationError> {
        if self.ran.contains(owner) || !self.in_progress.insert(owner.clone()) {
            return Ok(());
        }
        let result = self.run_block_inner(owner);
        self.in_progress.remove(owner);
        if result.is_ok() {
            self.ran.insert(owner.clone());
        }
        result
    }

    fn run_block_inner(&mut self, owner: &BlockRef) -> Result<(), EvaluationError> {
        let artifact = self.state.artifact();
        let Some(rule) = artifact.rule_by_ref.get(owner) else {
            return Ok(());
        };
        let rule = rule.clone();

        if let Some(plan) = artifact.read_plans.get(owner) {
            for path in plan.unconditional.iter() {
                self.demand(path)?;
            }
        }

        let picked = self.select(owner, &rule)?;
        for path in &self.state.demanded(owner, &picked) {
            self.demand(path)?;
        }
        self.commit(owner, &rule, picked)
    }

    pub(crate) fn select(&mut self, owner: &BlockRef, rule: &Block) -> Result<Picked, EvaluationError> {
        self.state.select(owner, rule, &mut self.isolate)
    }

    pub(crate) fn commit(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        picked: Picked,
    ) -> Result<(), EvaluationError> {
        self.state.commit(owner, rule, picked, &mut self.isolate)
    }
}

pub(crate) struct ExecutionContext<'a> {
    pub(crate) store: &'a Variable,
    pub(crate) policy_path: &'a Arc<str>,
    pub(crate) block_id: &'a Arc<str>,
    pub(crate) trace: bool,
    pub(crate) extras: bool,
    pub(crate) write_log: Option<&'a RefCell<Vec<WriteTrace>>>,
    pub(crate) env_mirror: Option<&'a Variable>,
    pub(crate) isolate: &'a RefCell<Isolate>,
}

impl<'a> ExecutionContext<'a> {
    pub(crate) fn of(site: &Site<'a>, isolate: &'a RefCell<Isolate>) -> Self {
        Self {
            store: site.store,
            policy_path: site.policy_path,
            block_id: site.block_id,
            trace: site.trace,
            extras: site.extras,
            write_log: site.write_log,
            env_mirror: site.env_mirror,
            isolate,
        }
    }


    pub(crate) fn expression_error(&self, expression: &Arc<str>, source: IsolateError) -> ExecutionError {
        ExecutionError {
            block_id: self.block_id.clone(),
            policy_path: self.policy_path.clone(),
            expression: expression.clone(),
            source,
        }
    }

    pub(crate) fn write(&self, path: &Arc<str>, value: Variable) {
        if let Some(log) = self.write_log {
            log.borrow_mut().push(WriteTrace {
                path: path.clone(),
                value: value.deep_clone(),
            });
        }
        HostState::write_into(self.store, self.env_mirror, path, value);
    }


}

impl Block {
    pub(crate) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        self.kind.execute(cx)
    }
}

impl BlockKind {
    pub(crate) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        match self {
            BlockKind::Assertion(a) => a.execute(cx),
            BlockKind::DecisionTable(d) => d.execute(cx),
            BlockKind::Expression(e) => e.execute(cx),
            BlockKind::Match(m) => m.execute(cx),
        }
    }
}

impl ExpressionIr {
    pub(crate) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        if self.key.is_empty() || self.value.is_empty() {
            return Ok(BlockTrace::Expression {
                property: self.key.clone(),
                value: Variable::Null,
            });
        }

        let mut isolate = cx.isolate.borrow_mut();
        let result = isolate
            .run_standard(&self.value)
            .map_err(|e| cx.expression_error(&self.value, e))?;
        let traced = cx.trace.then(|| result.deep_clone());
        cx.write(&self.key, result);

        Ok(BlockTrace::Expression {
            property: self.key.clone(),
            value: traced.unwrap_or(Variable::Null),
        })
    }
}

impl AssertionIr {
    pub(crate) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();
        let mut traces: Vec<ConditionTrace> = Vec::new();
        let result = self.evaluate_conditions(&mut isolate, cx, &mut traces)?;

        if !self.output.is_empty() {
            cx.write(&self.output, Variable::Bool(result));
        }

        Ok(BlockTrace::Assertion {
            result,
            conditions: traces,
        })
    }

    fn evaluate_conditions(
        &self,
        isolate: &mut Isolate,
        cx: &ExecutionContext,
        traces: &mut Vec<ConditionTrace>,
    ) -> Result<bool, ExecutionError> {
        let mut results = Vec::with_capacity(self.conditions.len());
        for condition in &self.conditions {
            let cond_result = isolate
                .run_standard(&condition.expression)
                .map_err(|e| cx.expression_error(&condition.expression, e))?
                .as_bool()
                .unwrap_or(false);
            traces.push(ConditionTrace {
                id: condition.id.clone(),
                result: cond_result,
            });
            results.push(cond_result);
        }
        Ok(self.fold(&results))
    }
}

impl MatchIr {
    pub(crate) fn select(&self, cx: &ExecutionContext) -> Result<MatchSelection, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();

        let mut arms: Vec<ConditionTrace> = Vec::new();
        let mut matched_arm: Option<Arc<str>> = None;

        for arm in &self.arms {
            if matched_arm.is_some() && !cx.extras {
                break;
            }
            let matches = if arm.condition.is_empty() {
                true
            } else {
                match isolate.run_standard(&arm.condition) {
                    Ok(value) => value.as_bool().unwrap_or(false),
                    Err(_) if cx.extras && matched_arm.is_some() => false,
                    Err(e) => return Err(cx.expression_error(&arm.condition, e)),
                }
            };
            arms.push(ConditionTrace {
                id: arm.id.clone(),
                result: matches,
            });
            if matches && matched_arm.is_none() {
                matched_arm = Some(arm.id.clone());
            }
        }

        Ok(MatchSelection { matched_arm, arms })
    }

    pub(crate) fn commit(
        &self,
        cx: &ExecutionContext,
        selection: &MatchSelection,
    ) -> Result<BlockTrace, ExecutionError> {
        let matched = selection
            .matched_arm
            .as_ref()
            .and_then(|id| self.arms.iter().find(|a| &a.id == id));
        let value = match matched {
            Some(arm) if !arm.value.is_empty() => {
                let mut isolate = cx.isolate.borrow_mut();
                isolate
                    .run_standard(&arm.value)
                    .map_err(|e| cx.expression_error(&arm.value, e))?
            }
            _ => Variable::Null,
        };

        let traced = cx.trace.then(|| value.deep_clone());
        if !self.key.is_empty() {
            cx.write(&self.key, value);
        }

        Ok(BlockTrace::Match {
            matched_arm: selection.matched_arm.clone(),
            value: traced.unwrap_or(Variable::Null),
            arms: if cx.trace {
                selection.arms.clone()
            } else {
                Vec::new()
            },
        })
    }

    pub(crate) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        let selection = self.select(cx)?;
        self.commit(cx, &selection)
    }
}

impl DecisionTableIr {
    pub(crate) fn select(&self, cx: &ExecutionContext) -> Result<TableSelection, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();
        let mut col_refs: Vec<Option<Variable>> = vec![None; self.inputs.len()];

        let active: Vec<_> = self
            .outputs
            .iter()
            .filter(|c| !c.field.is_empty())
            .collect();
        let has_collect = active.iter().any(|c| c.collect);
        let mut pending_scalars = active.iter().filter(|c| !c.collect).count();
        let mut taken: Vec<bool> = vec![false; active.len()];

        let mut matched_rows: Vec<u32> = Vec::new();
        let mut used_cells: Vec<(u32, Arc<str>)> = Vec::new();
        let cols = self.inputs.len();
        let bytes_per_row = cols.div_ceil(8);
        let mut input_bits: Option<Vec<u8>> = cx
            .extras
            .then(|| vec![0u8; self.rules.len() * bytes_per_row]);
        let candidates = match input_bits {
            None => self.candidate_rows(&mut isolate, &mut col_refs, cx)?,
            Some(_) => None,
        };

        for (row_idx, rule) in self.rules.iter().enumerate() {
            let satisfied = !has_collect && pending_scalars == 0 && !matched_rows.is_empty();
            if satisfied && !cx.extras {
                break;
            }

            let row_matches = match input_bits.as_mut() {
                Some(bits) => {
                    let per_col = self.evaluate_row_inputs_full(
                        rule,
                        &mut isolate,
                        &mut col_refs,
                        cx,
                        satisfied,
                    )?;
                    for (col_idx, &passed) in per_col.iter().enumerate() {
                        if passed {
                            let byte = row_idx * bytes_per_row + col_idx / 8;
                            bits[byte] |= 1 << (col_idx % 8);
                        }
                    }
                    per_col.iter().all(|p| *p)
                }
                None => match &candidates {
                    Some(rows) if !rows.contains(row_idx) => false,
                    Some(_) => self.evaluate_row_inputs_pruned(
                        row_idx,
                        rule,
                        &mut isolate,
                        &mut col_refs,
                        cx,
                    )?,
                    None => self.evaluate_row_inputs(rule, &mut isolate, &mut col_refs, cx)?,
                },
            };
            if !row_matches || satisfied {
                continue;
            }

            matched_rows.push(row_idx as u32);
            for (col_pos, col) in active.iter().enumerate() {
                if rule.get(&col.id).filter(|c| !c.is_empty()).is_none() {
                    continue;
                }
                if col.collect {
                    used_cells.push((row_idx as u32, col.id.clone()));
                } else if !taken[col_pos] {
                    taken[col_pos] = true;
                    used_cells.push((row_idx as u32, col.id.clone()));
                    pending_scalars -= 1;
                }
            }
        }

        Ok(TableSelection {
            matched_rows,
            used_cells,
            input_bits,
        })
    }

    pub(crate) fn commit(
        &self,
        cx: &ExecutionContext,
        selection: &TableSelection,
    ) -> Result<BlockTrace, ExecutionError> {
        let mut isolate = cx.isolate.borrow_mut();

        let used: HashSet<(u32, &str)> = selection
            .used_cells
            .iter()
            .map(|(row, col)| (*row, col.as_ref()))
            .collect();
        let mut collected: HashMap<Arc<str>, Vec<Variable>> = HashMap::default();
        let mut scalar_written: HashSet<Arc<str>> = HashSet::default();
        for col in &self.outputs {
            if col.collect && !col.field.is_empty() {
                collected.entry(col.field.clone()).or_default();
            }
        }

        let mut evaluations: Vec<HashMap<Arc<str>, Variable>> = Vec::new();
        for &row_idx in &selection.matched_rows {
            let Some(rule) = self.rules.get(row_idx as usize) else {
                continue;
            };
            let mut row_outputs: HashMap<Arc<str>, Variable> = HashMap::default();

            for col in &self.outputs {
                if !used.contains(&(row_idx, col.id.as_ref())) {
                    continue;
                }
                let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                    continue;
                };

                let value = isolate
                    .run_standard(cell)
                    .map_err(|e| cx.expression_error(cell, e))?;

                if cx.trace {
                    row_outputs.insert(col.field.clone(), value.deep_clone());
                }

                if col.collect {
                    collected.entry(col.field.clone()).or_default().push(value);
                } else {
                    cx.write(&col.field, value);
                    scalar_written.insert(col.field.clone());
                }
            }
            if cx.trace {
                evaluations.push(row_outputs);
            }
        }

        for col in &self.outputs {
            if !col.collect && !col.field.is_empty() && !scalar_written.contains(&col.field) {
                cx.write(&col.field, Variable::Null);
                scalar_written.insert(col.field.clone());
            }
        }
        for (field, values) in collected {
            cx.write(&field, Variable::from_array(values));
        }

        let extras = selection
            .input_bits
            .as_ref()
            .map(|bits| DecisionTableExtras {
                input_pass: base64::engine::general_purpose::STANDARD.encode(bits),
            });

        Ok(BlockTrace::DecisionTable {
            matched_rows: selection.matched_rows.clone(),
            evaluations,
            extras,
        })
    }

    pub(crate) fn execute(&self, cx: &ExecutionContext) -> Result<BlockTrace, ExecutionError> {
        let selection = self.select(cx)?;
        self.commit(cx, &selection)
    }

    fn candidate_rows(
        &self,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<Option<FixedBitSet>, ExecutionError> {
        let Some(index) = self.table_index() else {
            return Ok(None);
        };
        let mut acc: Option<FixedBitSet> = None;
        for (col_idx, column) in index.columns.iter().enumerate() {
            let Some(column) = column else {
                continue;
            };
            let Some(field) = self.inputs[col_idx]
                .field
                .as_ref()
                .filter(|f| !f.is_empty())
            else {
                continue;
            };
            let value = match &col_refs[col_idx] {
                Some(value) => value.shallow_clone(),
                None => {
                    let value = isolate
                        .run_standard(field)
                        .map_err(|e| cx.expression_error(field, e))?;
                    col_refs[col_idx] = Some(value.shallow_clone());
                    value
                }
            };
            if matches!(value, Variable::Dynamic(_)) {
                return Ok(None);
            }
            let mut col_rows = column.fallback.clone();
            if let Some(hit) = column.rows_for(&value) {
                col_rows.union_with(hit);
            }
            match &mut acc {
                None => acc = Some(col_rows),
                Some(set) => set.intersect_with(&col_rows),
            }
        }
        Ok(acc)
    }

    fn evaluate_row_inputs_pruned(
        &self,
        row_idx: usize,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<bool, ExecutionError> {
        let index = self.table_index();
        for (col_idx, col) in self.inputs.iter().enumerate() {
            if index.is_some_and(|ix| ix.decides(col_idx, row_idx)) {
                continue;
            }
            if !self.evaluate_cell(col_idx, col, rule, isolate, col_refs, cx)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn evaluate_row_inputs(
        &self,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<bool, ExecutionError> {
        for (col_idx, col) in self.inputs.iter().enumerate() {
            if !self.evaluate_cell(col_idx, col, rule, isolate, col_refs, cx)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn evaluate_row_inputs_full(
        &self,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
        row_unreached: bool,
    ) -> Result<Vec<bool>, ExecutionError> {
        let mut row_failed = false;
        self.inputs
            .iter()
            .enumerate()
            .map(|(col_idx, col)| {
                let passed = match self.evaluate_cell(col_idx, col, rule, isolate, col_refs, cx) {
                    Ok(passed) => passed,
                    Err(_) if row_unreached || row_failed => false,
                    Err(e) => return Err(e),
                };
                row_failed |= !passed;
                Ok(passed)
            })
            .collect()
    }

    fn evaluate_cell(
        &self,
        col_idx: usize,
        col: &DecisionTableInputField,
        rule: &HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate,
        col_refs: &mut [Option<Variable>],
        cx: &ExecutionContext,
    ) -> Result<bool, ExecutionError> {
        let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
            return Ok(true);
        };
        match col.field.as_ref() {
            Some(field) if !field.is_empty() => {
                let value = match &col_refs[col_idx] {
                    Some(value) => value.shallow_clone(),
                    None => {
                        let value = isolate
                            .run_standard(field)
                            .map_err(|e| cx.expression_error(field, e))?;
                        col_refs[col_idx] = Some(value.shallow_clone());
                        value
                    }
                };
                isolate
                    .set_reference_value(value)
                    .map_err(|e| cx.expression_error(field, e))?;
                isolate
                    .run_unary(cell)
                    .map_err(|e| cx.expression_error(cell, e))
            }
            _ => {
                let result = isolate
                    .run_standard(cell)
                    .map_err(|e| cx.expression_error(cell, e))?;
                Ok(result.as_bool().unwrap_or(false))
            }
        }
    }
}

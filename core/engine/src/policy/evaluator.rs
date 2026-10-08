use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use crate::compiled::policy::PolicyPlan;
use std::time::Instant;
use zen_types::symbol::Symbol;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::variable::Variable;
use zen_types::rccell::RcCell;

use zen_expression::{Isolate, OpcodeCache};

use crate::policy::blocks::{
    Block, BlockKind, BlockReadPlan, ExecutionContext, ExecutionError, MatchSelection,
    PropertyRead, TableSelection,
};
use crate::policy::ir::PropertyPath;
use crate::policy::queries::dependency::{DataModelPaths, EvalGraph, WriteScope};
use crate::policy::queries::path::PathClassifier;
use crate::policy::queries::scope::{EntityForm, EntitySources, ReferenceField};
use crate::policy::refs::RefPoolIndex;
use crate::policy::validator::InputSchema;
use crate::workspace::db::Db;
use crate::workspace::types::{
    BlockExecution, BlockRef, BlockTrace, EvaluateRequest, EvaluationError, EvaluationResult, Trace,
};

pub(crate) struct EvalArtifact {
    pub(crate) members: HashSet<Arc<str>>,
    pub(crate) eval_graph: EvalGraph,
    pub(crate) execution_order: Vec<PropertyPath>,
    pub(crate) entity_sources: Arc<EntitySources>,
    pub(crate) reference_fields: Vec<ReferenceField>,
    pub(crate) data_model_paths: DataModelPaths,
    pub(crate) classifier: PathClassifier,
    pub(crate) opcode_cache: Arc<OpcodeCache>,
    pub(crate) rule_by_ref: Arc<HashMap<BlockRef, Arc<Block>>>,
    pub(crate) input_schema: InputSchema,
    pub(crate) reads: HashMap<BlockRef, Arc<[PropertyRead]>>,
    pub(crate) read_plans: HashMap<BlockRef, BlockReadPlan>,
    pub(crate) compiled: OnceLock<Arc<PolicyPlan>>,
    pub(crate) goal_plans: Mutex<HashMap<Vec<Arc<str>>, Arc<PolicyPlan>>>,
    pub(crate) requirements: OnceLock<Requirements>,
}

pub(crate) type Iteration = (Arc<str>, Arc<str>, Option<Arc<str>>);

pub(crate) struct Requirements {
    order: Vec<PropertyPath>,
    goals: Vec<Arc<str>>,
    checks: Vec<Requirement>,
}

struct Requirement {
    path: PropertyPath,
    primary: Probe,
    alternate: Option<Probe>,
}

struct Probe {
    segments: Vec<Arc<str>>,
    optional: Vec<bool>,
    prefixes: Vec<String>,
}

impl Probe {
    fn new(paths: &DataModelPaths, path: &str) -> Self {
        let segments: Vec<Arc<str>> = path.split('.').map(Arc::from).collect();
        let prefixes = (0..segments.len())
            .map(|i| segments[..=i].iter().map(|s| s.as_ref()).collect::<Vec<_>>().join("."))
            .collect();
        Self {
            segments,
            optional: paths.optional_steps(path),
            prefixes,
        }
    }

    fn missing(&self, input: &Variable) -> bool {
        let mut current = input.shallow_clone();
        for (i, segment) in self.segments.iter().enumerate() {
            if current.as_object().is_none() {
                return false;
            }
            let next = current
                .as_object()
                .and_then(|o| o.borrow().get_str(segment).map(Variable::shallow_clone));
            match next {
                Some(Variable::Null) | None => return !self.optional[i..].iter().any(|o| *o),
                Some(v) => current = v,
            }
        }
        false
    }
}

struct Steps<'p> {
    probe: &'p Probe,
    steps: Vec<(Option<usize>, Vec<usize>)>,
}

impl<'p> Steps<'p> {
    fn new(probe: &'p Probe, columns: &zen_expression::lane::Columns) -> Self {
        let steps = probe
            .prefixes
            .iter()
            .map(|path| {
                let exact = columns.columns.iter().position(|(p, _)| *p == path.as_str());
                let under = columns
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(_, (p, _))| p.strip_prefix(path.as_str()).is_some_and(|rest| rest.starts_with('.')))
                    .map(|(at, _)| at)
                    .collect();
                (exact, under)
            })
            .collect();
        Self { probe, steps }
    }

    fn held(column: &zen_expression::lane::Column, row: usize) -> bool {
        use zen_expression::lane::Values;
        match column.values {
            Values::Any(_) | Values::Dict { .. } => column.valid(row) && !matches!(column.variable(row), Variable::Null),
            _ => column.valid(row),
        }
    }

    fn solid(column: &zen_expression::lane::Column, rows: usize) -> bool {
        use zen_expression::lane::Values;
        let full = column.validity.is_none_or(|(bits, offset)| Self::ones(bits, offset, rows));
        full && !matches!(column.values, Values::Any(_) | Values::Dict { .. })
    }

    fn ones(bits: &[u64], offset: usize, rows: usize) -> bool {
        (0..rows.div_ceil(64)).all(|w| {
            let take = (rows - (w << 6)).min(64);
            let mask = match take {
                64 => u64::MAX,
                _ => (1u64 << take) - 1,
            };
            zen_expression::lane::Column::word(bits, offset + (w << 6), take) & mask == mask
        })
    }

    fn always(&self, columns: &zen_expression::lane::Columns) -> bool {
        let rows = columns.rows;
        for (i, (exact, under)) in self.steps.iter().enumerate() {
            if let Some(at) = exact {
                let column = &columns.columns[*at].1;
                return Self::solid(column, rows) && (i + 1 == self.steps.len() || !matches!(column.values, zen_expression::lane::Values::Struct { .. }));
            }
            if !under.iter().any(|at| Self::solid(&columns.columns[*at].1, rows)) {
                return false;
            }
        }
        true
    }

    fn missing(&self, columns: &zen_expression::lane::Columns, row: usize) -> bool {
        let absent = |i: usize| !self.probe.optional[i..].iter().any(|o| *o);
        for (i, (exact, under)) in self.steps.iter().enumerate() {
            if let Some(at) = exact {
                let column = &columns.columns[*at].1;
                if !Self::held(column, row) {
                    return absent(i);
                }
                if i + 1 == self.steps.len() {
                    return false;
                }
                let nested = matches!(column.values, zen_expression::lane::Values::Any(_) | zen_expression::lane::Values::Struct { .. });
                return match nested.then(|| column.variable(row)).unwrap_or(Variable::Null) {
                    Variable::Object(_) => Probe {
                        segments: self.probe.segments[i + 1..].to_vec(),
                        optional: self.probe.optional[i + 1..].to_vec(),
                        prefixes: Vec::new(),
                    }
                    .missing(&column.variable(row)),
                    _ => false,
                };
            }
            if !under.iter().any(|at| Self::held(&columns.columns[*at].1, row)) {
                return absent(i);
            }
        }
        false
    }
}

impl Requirement {
    fn missing(&self, input: &Variable) -> bool {
        if !self.primary.missing(input) {
            return false;
        }
        self.alternate.as_ref().is_none_or(|alternate| alternate.missing(input))
    }
}

impl Db {
    pub fn evaluate(&self, req: &EvaluateRequest) -> Result<EvaluationResult, EvaluationError> {
        if self.is_graph(&req.policy_path) {
            return Err(EvaluationError::GraphNotEvaluable(req.policy_path.clone()));
        }
        if self.raw_policy(&req.policy_path).is_none() {
            return Err(EvaluationError::PolicyNotFound(req.policy_path.clone()));
        }
        self.check_imports_resolved(&req.policy_path)?;
        self.eval_artifact(&req.policy_path).evaluate(req, false)
    }

    pub fn enhance_trace(
        &self,
        req: &EvaluateRequest,
    ) -> Result<EvaluationResult, EvaluationError> {
        if self.is_graph(&req.policy_path) {
            return Err(EvaluationError::GraphNotEvaluable(req.policy_path.clone()));
        }
        if self.raw_policy(&req.policy_path).is_none() {
            return Err(EvaluationError::PolicyNotFound(req.policy_path.clone()));
        }
        self.check_imports_resolved(&req.policy_path)?;
        let mut req = req.clone();
        req.trace = true;
        self.eval_artifact(&req.policy_path).evaluate(&req, true)
    }

    pub fn evaluate_with_driver(
        &self,
        req: &EvaluateRequest,
    ) -> Result<EvaluationResult, EvaluationError> {
        self.check_evaluable(req)?;
        self.eval_artifact(&req.policy_path)
            .evaluate_with_driver(req, false)
    }

    pub fn evaluate_columns<'a>(
        &self,
        policy_path: &Arc<str>,
        goals: &[Arc<str>],
        columns: &'a zen_expression::lane::Columns<'a>,
    ) -> Result<crate::compiled::policy::PolicyColumnarOutput<'a>, EvaluationError> {
        let probe = EvaluateRequest {
            policy_path: policy_path.clone(),
            input: Variable::Null,
            goals: goals.to_vec(),
            trace: false,
        };
        self.check_evaluable(&probe)?;
        let artifact = self.eval_artifact(policy_path);
        Ok(artifact.plan(goals).evaluate_columns(&artifact, policy_path, goals, columns))
    }

    pub fn evaluate_batch(
        &self,
        requests: &[EvaluateRequest],
    ) -> Vec<Result<EvaluationResult, EvaluationError>> {
        let mut results: Vec<Option<Result<EvaluationResult, EvaluationError>>> =
            (0..requests.len()).map(|_| None).collect();
        let mut groups: Vec<(Arc<str>, Vec<usize>)> = Vec::new();
        for (index, request) in requests.iter().enumerate() {
            match self.check_evaluable(request) {
                Err(error) => results[index] = Some(Err(error)),
                Ok(()) => match groups.iter_mut().find(|(p, _)| *p == request.policy_path) {
                    Some((_, members)) => members.push(index),
                    None => groups.push((request.policy_path.clone(), vec![index])),
                },
            }
        }
        for (path, members) in groups {
            let artifact = self.eval_artifact(&path);
            let batch: Vec<EvaluateRequest> = members.iter().map(|&i| requests[i].clone()).collect();
            for (index, result) in members.into_iter().zip(artifact.evaluate_batch(&batch)) {
                results[index] = Some(result);
            }
        }
        results
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err(EvaluationError::PolicyNotFound(Arc::from("")))))
            .collect()
    }

    fn check_evaluable(&self, req: &EvaluateRequest) -> Result<(), EvaluationError> {
        if self.is_graph(&req.policy_path) {
            return Err(EvaluationError::GraphNotEvaluable(req.policy_path.clone()));
        }
        if self.raw_policy(&req.policy_path).is_none() {
            return Err(EvaluationError::PolicyNotFound(req.policy_path.clone()));
        }
        self.check_imports_resolved(&req.policy_path)
    }

    fn check_imports_resolved(&self, entry: &Arc<str>) -> Result<(), EvaluationError> {
        let mut visited: HashSet<Arc<str>> = HashSet::new();
        let mut queue: Vec<Arc<str>> = vec![entry.clone()];
        visited.insert(entry.clone());

        while let Some(path) = queue.pop() {
            let Some(parsed) = self.parsed(&path) else {
                continue;
            };
            for import in parsed.policy.imports() {
                if self.raw_policy(import).is_none() {
                    return Err(EvaluationError::ImportNotFound {
                        policy_path: path,
                        import: import.clone(),
                    });
                }
                if visited.insert(import.clone()) {
                    queue.push(import.clone());
                }
            }
        }
        Ok(())
    }
}

impl EvalArtifact {
    pub(crate) fn evaluate_entry(
        &self,
        key: &str,
        input: Variable,
        trace: bool,
    ) -> Result<EvaluationResult, EvaluationError> {
        let request = EvaluateRequest {
            policy_path: Arc::from(key),
            input,
            goals: Vec::new(),
            trace,
        };
        self.evaluate(&request, false)
    }

    pub(crate) fn evaluate(
        &self,
        req: &EvaluateRequest,
        extras: bool,
    ) -> Result<EvaluationResult, EvaluationError> {
        if !req.trace && !extras {
            if let Some(result) = self.plan(&req.goals).evaluate(self, std::slice::from_ref(req)).pop() {
                return result;
            }
        }
        self.evaluate_with_driver(req, extras)
    }

    const GOAL_PLANS: usize = 32;

    fn plan(&self, goals: &[Arc<str>]) -> Arc<PolicyPlan> {
        if goals.is_empty() {
            return self
                .compiled
                .get_or_init(|| {
                    Arc::new(PolicyPlan::compile(self, self.eval_graph.terminal_sinks(&self.members)))
                })
                .clone();
        }
        if let Some(plan) = self.goal_plans.lock().ok().and_then(|p| p.get(goals).cloned()) {
            return plan;
        }
        let plan = Arc::new(PolicyPlan::compile(self, goals.to_vec()));
        if let Ok(mut plans) = self.goal_plans.lock() {
            if plans.len() >= Self::GOAL_PLANS {
                plans.clear();
            }
            plans.insert(goals.to_vec(), plan.clone());
        }
        plan
    }

    pub(crate) fn evaluate_batch(
        &self,
        requests: &[EvaluateRequest],
    ) -> Vec<Result<EvaluationResult, EvaluationError>> {
        let mut results: Vec<Option<Result<EvaluationResult, EvaluationError>>> =
            (0..requests.len()).map(|_| None).collect();
        let mut groups: Vec<(&[Arc<str>], Vec<usize>)> = Vec::new();
        for (index, request) in requests.iter().enumerate() {
            match request.trace {
                true => results[index] = Some(self.evaluate_with_driver(request, false)),
                false => match groups.iter_mut().find(|(g, _)| *g == request.goals.as_slice()) {
                    Some((_, members)) => members.push(index),
                    None => groups.push((&request.goals, vec![index])),
                },
            }
        }
        for (goals, members) in groups {
            let batch: Vec<EvaluateRequest> = members.iter().map(|&i| requests[i].clone()).collect();
            for (index, result) in members.into_iter().zip(self.plan(goals).evaluate(self, &batch)) {
                results[index] = Some(result);
            }
        }
        results.into_iter().flatten().collect()
    }

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
            executions: driver.executions,
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

    pub(crate) fn prepare(
        &self,
        req: &EvaluateRequest,
    ) -> Result<(Variable, Vec<PropertyPath>), EvaluationError> {
        let input = self
            .input_schema
            .convert_dates(&req.input)
            .unwrap_or_else(|| req.input.clone());
        self.validate_request(req, &input)?;

        let store = input.depth_clone(1);
        let ref_targets: HashSet<Arc<str>> = self
            .reference_fields
            .iter()
            .map(|f| f.target.clone())
            .collect();
        let pool_index = RefPoolIndex::from_input(&store, ref_targets);
        store.hydrate_references(&self.reference_fields, &pool_index);

        let order_to_run = self.compute_order_to_run(req, &store)?;
        Ok((store, order_to_run))
    }

    pub(crate) fn roots(&self, req: &EvaluateRequest) -> Vec<Arc<str>> {
        match req.goals.is_empty() {
            true => self.eval_graph.terminal_sinks(&self.members),
            false => req.goals.clone(),
        }
    }

    fn validate_request(
        &self,
        req: &EvaluateRequest,
        input: &Variable,
    ) -> Result<(), EvaluationError> {
        for goal in &req.goals {
            if !self.eval_graph.contains(goal) {
                return Err(EvaluationError::GoalNotFound(goal.clone()));
            }
        }
        let validation_errors = self.input_schema.validate(input);
        if !validation_errors.is_empty() {
            return Err(EvaluationError::InputValidationFailed {
                errors: validation_errors,
            });
        }
        Ok(())
    }

    pub(crate) fn sure(&self, columns: &zen_expression::lane::Columns, goals: &[Arc<str>]) -> Option<Vec<bool>> {
        if !goals.is_empty() || !self.reference_fields.is_empty() {
            return None;
        }
        let mut sure = self.input_schema.sure(columns)?;
        for check in &self.requirements().checks {
            let primary = Steps::new(&check.primary, columns);
            if primary.always(columns) {
                continue;
            }
            let alternate = check.alternate.as_ref().map(|a| Steps::new(a, columns));
            if alternate.as_ref().is_some_and(|a| a.always(columns)) {
                continue;
            }
            for (row, sure) in sure.iter_mut().enumerate() {
                if *sure && primary.missing(columns, row) && alternate.as_ref().is_none_or(|a| a.missing(columns, row)) {
                    *sure = false;
                }
            }
        }
        Some(sure)
    }

    fn requirements(&self) -> &Requirements {
        self.requirements.get_or_init(|| {
            let visible = &self.members;
            let order: Vec<PropertyPath> = self
                .execution_order
                .iter()
                .filter(|path| {
                    self.eval_graph
                        .writer_for(path)
                        .is_some_and(|o| visible.contains(&o.policy_path))
                })
                .cloned()
                .collect();
            let goals = self.eval_graph.terminal_sinks(visible);
            let entity_form = EntityForm::new(&self.entity_sources);
            let checks = self
                .eval_graph
                .reachable_input_paths(&goals, visible)
                .into_iter()
                .filter(|p| {
                    !entity_form
                        .rewrite(p)
                        .is_some_and(|entity| self.eval_graph.written_at(&entity, visible))
                })
                .map(|path| {
                    let alternate = path.split_once('.').and_then(|(entity, rest)| {
                        let src = self.entity_sources.get(entity)?;
                        Some(Probe::new(&self.data_model_paths, &format!("{}.{}", src.path, rest)))
                    });
                    Requirement {
                        primary: Probe::new(&self.data_model_paths, &path),
                        alternate,
                        path,
                    }
                })
                .collect();
            Requirements { order, goals, checks }
        })
    }

    fn compute_order_to_run(
        &self,
        req: &EvaluateRequest,
        input: &Variable,
    ) -> Result<Vec<PropertyPath>, EvaluationError> {
        if req.goals.is_empty() {
            let requirements = self.requirements();
            let mut missing: Vec<PropertyPath> = requirements
                .checks
                .iter()
                .filter(|check| check.missing(input))
                .map(|check| check.path.clone())
                .collect();
            if !missing.is_empty() {
                missing.sort();
                return Err(EvaluationError::MissingRequiredInputs {
                    goals: requirements.goals.clone(),
                    missing,
                });
            }
            return Ok(requirements.order.clone());
        }

        let visible = &self.members;
        let visible_order: Vec<PropertyPath> = self
            .execution_order
            .iter()
            .filter(|path| {
                self.eval_graph
                    .writer_for(path)
                    .is_some_and(|o| visible.contains(&o.policy_path))
            })
            .cloned()
            .collect();

        let goals = match req.goals.is_empty() {
            true => self.eval_graph.terminal_sinks(visible),
            false => req.goals.clone(),
        };
        let entity_form = EntityForm::new(&self.entity_sources);
        let mut missing: Vec<PropertyPath> = self
            .eval_graph
            .reachable_input_paths(&goals, visible)
            .into_iter()
            .filter(|p| {
                !entity_form
                    .rewrite(p)
                    .is_some_and(|entity| self.eval_graph.written_at(&entity, visible))
            })
            .filter(|p| self.input_missing(input, p))
            .collect();
        if !missing.is_empty() {
            missing.sort();
            return Err(EvaluationError::MissingRequiredInputs { goals, missing });
        }

        if req.goals.is_empty() {
            return Ok(visible_order);
        }

        let reachable = self.eval_graph.reachable_from(&req.goals);

        Ok(visible_order
            .iter()
            .filter(|p| reachable.contains(*p))
            .cloned()
            .collect())
    }

    fn input_missing(&self, input: &Variable, path: &str) -> bool {
        if !self.path_missing(input, path) {
            return false;
        }
        let Some((entity, rest)) = path.split_once('.') else {
            return true;
        };
        match self.entity_sources.get(entity) {
            Some(src) => self.path_missing(input, &format!("{}.{}", src.path, rest)),
            None => true,
        }
    }

    fn path_missing(&self, input: &Variable, path: &str) -> bool {
        let optional = self.data_model_paths.optional_steps(path);
        let mut current = input.shallow_clone();
        for (i, segment) in path.split('.').enumerate() {
            if current.as_object().is_none() {
                return false;
            }
            match current.dot(segment) {
                Some(Variable::Null) | None => return !optional[i..].iter().any(|o| *o),
                Some(v) => current = v,
            }
        }
        false
    }
}

pub(crate) struct Driver<'a> {
    artifact: &'a EvalArtifact,
    store: &'a Variable,
    env: Variable,
    entry: &'a Arc<str>,
    trace: bool,
    extras: bool,
    isolate: Rc<RefCell<Isolate>>,
    ran: HashSet<BlockRef>,
    in_progress: HashSet<BlockRef>,
    executions: Vec<BlockExecution>,
}

pub(crate) enum Pick {
    Unconditional,
    Match(MatchSelection),
    Table(TableSelection),
}

impl Pick {
    pub(crate) fn collect_reads(&self, plan: &BlockReadPlan, out: &mut Vec<Arc<str>>) {
        match self {
            Pick::Match(selection) => {
                if let Some(arm_id) = &selection.matched_arm {
                    if let Some(reads) = plan.match_arm_reads(arm_id) {
                        out.extend(reads.iter().cloned());
                    }
                }
            }
            Pick::Table(selection) => {
                for (row_idx, col_id) in &selection.used_cells {
                    out.extend(plan.cell_reads(*row_idx, col_id));
                }
            }
            Pick::Unconditional => {}
        }
    }
}

struct PhaseScope {
    scoped: Variable,
    entity_key: Rc<str>,
}

impl PhaseScope {
    fn new(store: &Variable, entity_key: Rc<str>) -> Self {
        Self {
            scoped: store.depth_clone(1),
            entity_key,
        }
    }

    fn bind(
        &self,
        instance: &Variable,
        owner_binding: &Option<(String, Variable)>,
    ) -> Option<InstanceSlot> {
        let scoped_fields = self.scoped.as_object()?;

        let needs_owner = match (owner_binding, instance.as_object()) {
            (Some((name, _)), Some(fields)) => {
                !fields.borrow().contains_key(&Symbol::from(name.as_str()))
            }
            _ => false,
        };

        let (bound, slot) = if needs_owner {
            let wrapper = instance.depth_clone(1);
            let synthetic_owner = match (owner_binding, wrapper.as_object()) {
                (Some((name, owner_var)), Some(wrapper_fields)) => {
                    let key = Symbol::from(name.as_str());
                    let injected = owner_var.shallow_clone();
                    wrapper_fields
                        .borrow_mut()
                        .insert(key.clone(), injected.shallow_clone());
                    Some((key, injected))
                }
                _ => None,
            };
            let bound = wrapper.shallow_clone();
            (
                bound,
                InstanceSlot::Wrapped {
                    wrapper,
                    synthetic_owner,
                },
            )
        } else {
            (instance.shallow_clone(), InstanceSlot::Direct)
        };

        {
            let mut fields = scoped_fields.borrow_mut();
            fields.remove(&Variable::dollar_key());
            fields.insert(Symbol::from(self.entity_key.as_ref()), bound);
        }
        Some(slot)
    }
}

pub(crate) enum InstanceSlot {
    Direct,
    Wrapped {
        wrapper: Variable,
        synthetic_owner: Option<(Symbol, Variable)>,
    },
}

impl InstanceSlot {
    pub(crate) fn write_back(&self, instance: &Variable) {
        let Self::Wrapped {
            wrapper,
            synthetic_owner,
        } = self
        else {
            return;
        };
        let (Some(written), Some(target)) = (wrapper.as_object(), instance.as_object()) else {
            return;
        };
        let written = written.borrow();
        let mut target = target.borrow_mut();
        for (key, value) in written.iter() {
            if Self::is_injected_owner(synthetic_owner, key.clone(), value) {
                continue;
            }
            target.insert(key.clone(), value.shallow_clone());
        }
    }

    fn is_injected_owner(
        synthetic_owner: &Option<(Symbol, Variable)>,
        key: Symbol,
        value: &Variable,
    ) -> bool {
        match synthetic_owner {
            Some((owner_key, injected)) if owner_key.as_str() == key.as_str() => {
                Self::same_ref(value, injected)
            }
            _ => false,
        }
    }

    fn same_ref(a: &Variable, b: &Variable) -> bool {
        match (a, b) {
            (Variable::Object(x), Variable::Object(y)) => RcCell::ptr_eq(x, y),
            (Variable::Array(x), Variable::Array(y)) => RcCell::ptr_eq(x, y),
            (Variable::String(x), Variable::String(y)) => x == y,
            _ => a == b,
        }
    }
}

pub(crate) enum Picked {
    Skipped,
    Singleton(Pick),
    Iterated(Iterated),
}

impl Iterated {
    pub(crate) fn picks(&self) -> &[Pick] {
        &self.picks
    }

    pub(crate) fn instances(&self) -> &[Variable] {
        &self.instances
    }

    pub(crate) fn picked(mut self, picks: Vec<Pick>) -> Picked {
        self.picks = picks;
        Picked::Iterated(self)
    }
}

pub(crate) struct Iterated {
    entity: Rc<str>,
    iter_path: Arc<str>,
    instances: Vec<Variable>,
    single: bool,
    owner_binding: Option<(String, Variable)>,
    picks: Vec<Pick>,
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
            isolate: Rc::new(RefCell::new(
                Isolate::new().with_cache(Some(artifact.opcode_cache.clone())),
            )),
            artifact,
            store,
            env: store.depth_clone(1),
            entry,
            trace,
            extras,
            ran: HashSet::new(),
            in_progress: HashSet::new(),
            executions: Vec::new(),
        }
    }

    fn bind_env(&self, isolate: &RefCell<Isolate>) {
        if let Some(fields) = self.env.as_object() {
            fields.borrow_mut().remove(&Variable::dollar_key());
        }
        isolate
            .borrow_mut()
            .set_environment(self.env.shallow_clone());
    }

    pub(crate) fn demand(&mut self, prop: &str) -> Result<(), EvaluationError> {
        self.writers_of_longest_prefix(prop)
            .iter()
            .try_for_each(|owner| self.run_block(owner))
    }

    pub(crate) fn writers_of_longest_prefix(&self, prop: &str) -> &'a [BlockRef] {
        self.artifact.writers_of_longest_prefix(prop)
    }

    pub(crate) fn env(&self) -> &Variable {
        &self.env
    }

    pub(crate) fn clear_dollar(&self) {
        if let Some(fields) = self.env.as_object() {
            fields.borrow_mut().remove(&Variable::dollar_key());
        }
    }

    pub(crate) fn write(&self, path: &str, value: Variable) {
        ExecutionContext::write_into(self.store, Some(&self.env), path, value);
    }
}

impl EvalArtifact {
    pub(crate) fn iteration(&self, rule: &Block) -> Option<Iteration> {
        match rule.write_scope(&self.classifier) {
            WriteScope::Entity(entity) => self
                .entity_sources
                .get(entity.as_ref())
                .map(|src| (entity, src.path.clone(), src.owner.clone())),
            _ => None,
        }
    }

    pub(crate) fn writers_of_longest_prefix(&self, prop: &str) -> &[BlockRef] {
        let graph = &self.eval_graph;
        let direct = graph.demand_writers_for(prop);
        if !direct.is_empty() {
            return direct;
        }
        let mut end = prop.len();
        while let Some(dot) = prop[..end].rfind('.') {
            let owners = graph.demand_writers_for(&prop[..dot]);
            if !owners.is_empty() {
                return owners;
            }
            end = dot;
        }
        &[]
    }
}

impl<'a> Driver<'a> {
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
        let artifact = self.artifact;
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
        for path in &self.demanded(owner, &picked) {
            self.demand(path)?;
        }
        self.commit(owner, &rule, picked)
    }

    pub(crate) fn iterated(&self, rule: &Block) -> Option<Iteration> {
        self.artifact.iteration(rule)
    }

    pub(crate) fn select(&mut self, owner: &BlockRef, rule: &Block) -> Result<Picked, EvaluationError> {
        match self.iterated(rule) {
            Some((entity, path, src_owner)) => {
                self.select_iterated(owner, rule, entity.as_ref(), &path, src_owner.as_deref())
            }
            None => self.select_singleton(owner, rule).map(Picked::Singleton),
        }
    }

    pub(crate) fn demanded(&self, owner: &BlockRef, picked: &Picked) -> Vec<Arc<str>> {
        let mut demanded: Vec<Arc<str>> = Vec::new();
        let Some(plan) = self.artifact.read_plans.get(owner) else {
            return demanded;
        };
        match picked {
            Picked::Skipped => {}
            Picked::Singleton(pick) => pick.collect_reads(plan, &mut demanded),
            Picked::Iterated(iterated) => {
                for pick in &iterated.picks {
                    pick.collect_reads(plan, &mut demanded);
                }
                demanded.sort();
                demanded.dedup();
            }
        }
        demanded
    }

    pub(crate) fn commit(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        picked: Picked,
    ) -> Result<(), EvaluationError> {
        match picked {
            Picked::Skipped => Ok(()),
            Picked::Singleton(pick) => self.commit_singleton(owner, rule, pick),
            Picked::Iterated(iterated) => self.commit_iterated(owner, rule, iterated),
        }
    }

    fn select_pick(rule: &Block, ctx: &ExecutionContext) -> Result<Pick, ExecutionError> {
        match &rule.kind {
            BlockKind::Match(m) => m.select(ctx).map(Pick::Match),
            BlockKind::DecisionTable(d) => d.select(ctx).map(Pick::Table),
            BlockKind::Expression(_) | BlockKind::Assertion(_) => Ok(Pick::Unconditional),
        }
    }

    fn commit_pick(
        rule: &Block,
        ctx: &ExecutionContext,
        pick: &Pick,
    ) -> Result<BlockTrace, ExecutionError> {
        match (&rule.kind, pick) {
            (BlockKind::Match(m), Pick::Match(selection)) => m.commit(ctx, selection),
            (BlockKind::DecisionTable(d), Pick::Table(selection)) => d.commit(ctx, selection),
            _ => rule.execute(ctx),
        }
    }

    fn select_singleton(&mut self, owner: &BlockRef, rule: &Block) -> Result<Pick, EvaluationError> {
        let isolate = Rc::clone(&self.isolate);
        let env = self.env.shallow_clone();
        let ctx = ExecutionContext {
            store: self.store,
            policy_path: &owner.policy_path,
            block_id: &rule.id,
            trace: self.trace,
            extras: self.extras,
            write_log: None,
            env_mirror: Some(&env),
            isolate: &isolate,
        };
        if matches!(rule.kind, BlockKind::Match(_) | BlockKind::DecisionTable(_)) {
            self.bind_env(&isolate);
        }
        Ok(Self::select_pick(rule, &ctx)?)
    }

    fn commit_singleton(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        pick: Pick,
    ) -> Result<(), EvaluationError> {
        let write_log = (self.trace && self.extras).then(|| RefCell::new(Vec::new()));
        let isolate = Rc::clone(&self.isolate);
        let env = self.env.shallow_clone();
        let ctx = ExecutionContext {
            store: self.store,
            policy_path: &owner.policy_path,
            block_id: &rule.id,
            trace: self.trace,
            extras: self.extras,
            write_log: write_log.as_ref(),
            env_mirror: Some(&env),
            isolate: &isolate,
        };
        self.bind_env(&isolate);
        let bt = Self::commit_pick(rule, &ctx, &pick)?;

        if self.trace {
            let trace_policy_path =
                (&owner.policy_path != self.entry).then(|| owner.policy_path.clone());
            let operand_values =
                Block::operand_values(self.extras, self.reads_for(owner), self.store);
            self.executions.push(BlockExecution {
                block_id: rule.id.clone(),
                policy_path: trace_policy_path,
                instance_path: None,
                trace: bt,
                operand_values,
                writes: write_log.map(RefCell::into_inner).unwrap_or_default(),
                reads: self.execution_reads(owner, &pick),
            });
        }
        Ok(())
    }

    fn captured(&self, entity: &str, iter_path: &Arc<str>, owner_name: Option<&str>) -> Option<Iterated> {
        let target = self.store.dot(iter_path.as_ref())?;
        let single = target.as_object().is_some();
        let instances: Vec<Variable> = match target.as_array() {
            Some(arr) => arr.borrow().iter().map(|v| v.shallow_clone()).collect(),
            None if single => vec![target.shallow_clone()],
            None => return None,
        };
        let owner_binding = owner_name.and_then(|name| {
            let owner_path = iter_path.rsplit_once('.').map(|(o, _)| o)?;
            self.store
                .dot(owner_path)
                .map(|var| (name.to_string(), var))
        });
        Some(Iterated {
            entity: Rc::from(entity),
            iter_path: iter_path.clone(),
            instances,
            single,
            owner_binding,
            picks: Vec::new(),
        })
    }

    pub(crate) fn capture(&self, rule: &Block) -> Option<Iterated> {
        let (entity, path, owner) = self.iterated(rule)?;
        self.captured(entity.as_ref(), &path, owner.as_deref())
    }

    pub(crate) fn instance_scopes(&self, iterated: &Iterated) -> Vec<Option<(Variable, InstanceSlot)>> {
        iterated
            .instances
            .iter()
            .map(|instance| {
                let phase = PhaseScope::new(self.store, iterated.entity.clone());
                let slot = phase.bind(instance, &iterated.owner_binding)?;
                Some((phase.scoped, slot))
            })
            .collect()
    }

    fn select_iterated(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        entity: &str,
        iter_path: &Arc<str>,
        owner_name: Option<&str>,
    ) -> Result<Picked, EvaluationError> {
        let Some(captured) = self.captured(entity, iter_path, owner_name) else {
            return Ok(Picked::Skipped);
        };
        let Iterated {
            entity: entity_key,
            instances,
            single,
            owner_binding,
            ..
        } = captured;

        let picks: Vec<Pick> =
            if matches!(rule.kind, BlockKind::Match(_) | BlockKind::DecisionTable(_)) {
                let phase = PhaseScope::new(self.store, entity_key.clone());
                let isolate = Rc::clone(&self.isolate);
                isolate
                    .borrow_mut()
                    .set_environment(phase.scoped.shallow_clone());
                let mut picks = Vec::with_capacity(instances.len());
                for instance in &instances {
                    let Some(_slot) = phase.bind(instance, &owner_binding) else {
                        picks.push(Pick::Unconditional);
                        continue;
                    };
                    let ctx = ExecutionContext {
                        store: &phase.scoped,
                        policy_path: &owner.policy_path,
                        block_id: &rule.id,
                        trace: self.trace,
                        extras: self.extras,
                        write_log: None,
                        env_mirror: None,
                        isolate: &isolate,
                    };
                    picks.push(Self::select_pick(rule, &ctx)?);
                }
                picks
            } else {
                instances.iter().map(|_| Pick::Unconditional).collect()
            };

        Ok(Picked::Iterated(Iterated {
            entity: entity_key,
            iter_path: iter_path.clone(),
            instances,
            single,
            owner_binding,
            picks,
        }))
    }

    fn commit_iterated(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        iterated: Iterated,
    ) -> Result<(), EvaluationError> {
        let Iterated {
            entity: entity_key,
            iter_path,
            instances,
            single,
            owner_binding,
            picks,
        } = iterated;
        let trace_policy_path =
            (&owner.policy_path != self.entry).then(|| owner.policy_path.clone());
        let phase = PhaseScope::new(self.store, entity_key);
        let isolate = Rc::clone(&self.isolate);
        isolate
            .borrow_mut()
            .set_environment(phase.scoped.shallow_clone());
        for (idx, instance) in instances.iter().enumerate() {
            let Some(slot) = phase.bind(instance, &owner_binding) else {
                continue;
            };
            let write_log = (self.trace && self.extras).then(|| RefCell::new(Vec::new()));
            let ctx = ExecutionContext {
                store: &phase.scoped,
                policy_path: &owner.policy_path,
                block_id: &rule.id,
                trace: self.trace,
                extras: self.extras,
                write_log: write_log.as_ref(),
                env_mirror: None,
                isolate: &isolate,
            };
            let result = Self::commit_pick(rule, &ctx, &picks[idx]);
            let operand_values = match (&result, self.trace) {
                (Ok(_), true) => {
                    Block::operand_values(self.extras, self.reads_for(owner), &phase.scoped)
                }
                _ => HashMap::default(),
            };
            slot.write_back(instance);
            let bt = result?;
            if self.trace {
                self.executions.push(BlockExecution {
                    block_id: rule.id.clone(),
                    policy_path: trace_policy_path.clone(),
                    instance_path: Some(match single {
                        true => iter_path.clone(),
                        false => format!("{iter_path}.{idx}").into(),
                    }),
                    trace: bt,
                    operand_values,
                    writes: write_log.map(RefCell::into_inner).unwrap_or_default(),
                    reads: self.execution_reads(owner, &picks[idx]),
                });
            }
        }
        Ok(())
    }

    fn reads_for(&self, owner: &BlockRef) -> &[PropertyRead] {
        if self.extras {
            self.artifact
                .reads
                .get(owner)
                .map(|r| r.as_ref())
                .unwrap_or(&[])
        } else {
            &[]
        }
    }

    fn execution_reads(&self, owner: &BlockRef, pick: &Pick) -> Vec<Arc<str>> {
        if !self.extras {
            return Vec::new();
        }
        let Some(plan) = self.artifact.read_plans.get(owner) else {
            return Vec::new();
        };
        let mut reads: Vec<Arc<str>> = plan.unconditional.to_vec();
        pick.collect_reads(plan, &mut reads);
        reads.sort();
        reads.dedup();
        reads
    }
}

impl Block {
    fn operand_values(
        extras: bool,
        reads: &[PropertyRead],
        store: &Variable,
    ) -> HashMap<Arc<str>, Variable> {
        let mut out: HashMap<Arc<str>, Variable> = HashMap::new();
        if !extras {
            return out;
        }
        for read in reads {
            if read.via_alias || read.unresolved {
                continue;
            }
            if let Some(value) = store.dot(&read.path) {
                out.entry(read.path.clone())
                    .or_insert_with(|| value.deep_clone());
            }
        }
        out
    }
}

trait StoreOps {
    fn hydrate_references(&self, reference_fields: &[ReferenceField], pool_index: &RefPoolIndex);
    fn snapshot(&self, order: &[PropertyPath]) -> HashMap<Arc<str>, Variable>;
}

impl StoreOps for Variable {
    fn hydrate_references(&self, reference_fields: &[ReferenceField], pool_index: &RefPoolIndex) {
        for field in reference_fields {
            let Some(lookup) = pool_index.pool_for(field.target.as_ref()) else {
                continue;
            };
            let Some(ref_var) = self.dot(field.path.as_ref()) else {
                continue;
            };

            if field.array {
                let Some(ref_arr) = ref_var.as_array() else {
                    continue;
                };
                let mut borrowed = ref_arr.borrow_mut();
                for slot in borrowed.iter_mut() {
                    if let Some(id) = slot.as_rc_str() {
                        if let Some(obj) = lookup.get(&id) {
                            *slot = obj.shallow_clone();
                        }
                    }
                }
            } else if let Some(id) = ref_var.as_rc_str() {
                if let Some(obj) = lookup.get(&id) {
                    self.dot_insert(field.path.as_ref(), obj.shallow_clone());
                }
            }
        }
    }

    fn snapshot(&self, order: &[PropertyPath]) -> HashMap<Arc<str>, Variable> {
        let mut props = HashMap::new();
        for path in order {
            if let Some(val) = self.dot(path) {
                props.insert(path.clone(), val.deep_clone());
            }
        }
        props
    }
}

impl From<ExecutionError> for EvaluationError {
    fn from(e: ExecutionError) -> Self {
        Self::ExpressionFailed {
            policy_path: e.policy_path,
            block_id: e.block_id,
            expression: e.expression,
            source: e.source,
            partial_trace: None,
        }
    }
}

impl EvaluationError {
    fn with_partial_trace(self, trace: Option<Trace>) -> Self {
        match (self, trace) {
            (
                EvaluationError::ExpressionFailed {
                    policy_path,
                    block_id,
                    expression,
                    source,
                    ..
                },
                Some(trace),
            ) => EvaluationError::ExpressionFailed {
                policy_path,
                block_id,
                expression,
                source,
                partial_trace: Some(Box::new(trace)),
            },
            (other, _) => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EvalArtifact;

    const fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn eval_artifact_is_send_sync() {
        assert_send_sync::<EvalArtifact>();
    }
}

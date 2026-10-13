use std::sync::{Arc, Mutex, OnceLock};
use crate::compiled::policy::{Host, PolicyPlan};

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::variable::Variable;

use zen_expression::lane::Programs;

use crate::policy::blocks::{Block, BlockReadPlan, ExecutionError, PropertyRead};
use crate::policy::ir::PropertyPath;
use crate::policy::queries::dependency::{DataModelPaths, EvalGraph, WriteScope};
use crate::policy::queries::path::PathClassifier;
use crate::policy::queries::scope::{EntityForm, EntitySources, ReferenceField};
use crate::policy::refs::RefPoolIndex;
use crate::policy::validator::InputSchema;
use crate::workspace::db::Db;
use crate::workspace::types::{BlockRef, EvaluateRequest, EvaluationError, EvaluationResult, Trace};

pub(crate) struct EvalArtifact {
    pub(crate) members: HashSet<Arc<str>>,
    pub(crate) eval_graph: EvalGraph,
    pub(crate) execution_order: Vec<PropertyPath>,
    pub(crate) entity_sources: Arc<EntitySources>,
    pub(crate) reference_fields: Vec<ReferenceField>,
    pub(crate) data_model_paths: DataModelPaths,
    pub(crate) classifier: PathClassifier,
    #[cfg(feature = "driver")]
    pub(crate) opcode_cache: Arc<zen_expression::OpcodeCache>,
    pub(crate) rule_by_ref: Arc<HashMap<BlockRef, Arc<Block>>>,
    pub(crate) input_schema: InputSchema,
    pub(crate) reads: HashMap<BlockRef, Arc<[PropertyRead]>>,
    pub(crate) read_plans: HashMap<BlockRef, BlockReadPlan>,
    pub(crate) compiled: OnceLock<Arc<PolicyPlan>>,
    pub(crate) goal_plans: Mutex<HashMap<Vec<Arc<str>>, Arc<PolicyPlan>>>,
    pub(crate) requirements: OnceLock<Requirements>,
    pub(crate) programs: OnceLock<Arc<Programs>>,
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

    fn coded(column: &zen_expression::lane::Column) -> Option<usize> {
        use zen_expression::lane::{Dictionary, Values};
        match column.values {
            Values::Dict {
                values: values @ (Dictionary::Text { .. } | Dictionary::Scaled { .. } | Dictionary::Bool { .. }),
                ..
            } => Some(values.len()),
            _ => None,
        }
    }

    fn held(column: &zen_expression::lane::Column, row: usize) -> bool {
        use zen_expression::lane::Values;
        match (column.values, Self::coded(column)) {
            (Values::Dict { .. }, Some(len)) => column.valid(row) && column.code(row).is_some_and(|key| key < len),
            (Values::Any(_) | Values::Dict { .. }, _) => column.valid(row) && !matches!(column.variable(row), Variable::Null),
            _ => column.valid(row),
        }
    }

    fn solid(column: &zen_expression::lane::Column, rows: usize) -> bool {
        use zen_expression::lane::Values;
        let full = column.validity.is_none_or(|(bits, offset)| Self::ones(bits, offset, rows));
        match (column.values, Self::coded(column)) {
            (Values::Dict { keys, .. }, Some(len)) => {
                full && keys.get(..rows).is_some_and(|keys| keys.iter().all(|&key| usize::try_from(key).is_ok_and(|key| key < len)))
            }
            (values, _) => full && !matches!(values, Values::Any(_) | Values::Dict { .. }),
        }
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

    pub fn evaluate_columns<'a>(
        &self,
        policy_path: &Arc<str>,
        goals: &[Arc<str>],
        columns: &'a zen_expression::lane::Columns<'a>,
        store: &'a crate::compiled::policy::ColumnStore<'a>,
    ) -> Result<crate::compiled::policy::PolicyColumnarOutput<'a>, EvaluationError> {
        let probe = EvaluateRequest {
            policy_path: policy_path.clone(),
            input: Variable::Null,
            goals: goals.to_vec(),
            trace: false,
        };
        self.check_evaluable(&probe)?;
        Ok(self.eval_artifact(policy_path).evaluate_columns(policy_path, goals, columns, store))
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

    pub(crate) fn check_evaluable(&self, req: &EvaluateRequest) -> Result<(), EvaluationError> {
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
        let plan = self.plan(&req.goals);
        if !req.trace && !extras {
            if let Some(result) = plan.evaluate(self, std::slice::from_ref(req)).pop() {
                return result;
            }
        }
        plan.traced(self, req, extras)
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

    /// The policy's `goals` (every goal when none) over columnar input, on
    /// its compiled plan: the input first prepared as columns (dates,
    /// defaults, references) in `store` when the policy converts it and its
    /// shape allows; else each row prepared as it is evaluated.
    pub(crate) fn evaluate_columns<'a>(
        &self,
        policy_path: &Arc<str>,
        goals: &[Arc<str>],
        columns: &'a zen_expression::lane::Columns<'a>,
        store: &'a crate::compiled::policy::ColumnStore<'a>,
    ) -> crate::compiled::policy::PolicyColumnarOutput<'a> {
        use crate::compiled::policy::columnar::prepare;
        let converts = !self.reference_fields.is_empty() || self.input_schema.converts();
        let prepared = converts
            .then(|| prepare::prepare(self, columns, store))
            .flatten()
            .map(|prepared| store.keep(prepared));
        self.plan(goals).evaluate_columns(self, policy_path, goals, columns, prepared)
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
                true => results[index] = Some(self.plan(&request.goals).traced(self, request, false)),
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
        let sure = self.input_schema.sure(columns)?;
        Some(self.required(columns, sure))
    }

    /// The prepared rows certain to pass: validated as `converted`, their
    /// requirements met in `hydrated` (as the row path checks each).
    pub(crate) fn sure_prepared(
        &self,
        converted: &zen_expression::lane::Columns,
        hydrated: &zen_expression::lane::Columns,
        goals: &[Arc<str>],
    ) -> Option<Vec<bool>> {
        if !goals.is_empty() {
            return None;
        }
        let sure = self.input_schema.sure_prepared(converted)?;
        Some(self.required(hydrated, sure))
    }

    fn required(&self, columns: &zen_expression::lane::Columns, mut sure: Vec<bool>) -> Vec<bool> {
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
        sure
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

    pub(crate) fn programs(&self) -> Arc<Programs> {
        self.programs
            .get_or_init(|| {
                Arc::new(Host::programs(
                    self.rule_by_ref
                        .iter()
                        .filter(|(owner, _)| self.members.contains(&owner.policy_path))
                        .map(|(_, rule)| rule.as_ref()),
                ))
            })
            .clone()
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

pub(crate) trait StoreOps {
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
    pub(crate) fn with_partial_trace(self, trace: Option<Trace>) -> Self {
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

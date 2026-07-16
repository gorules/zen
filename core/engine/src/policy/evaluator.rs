use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::variable::Variable;

use zen_expression::{Isolate, OpcodeCache};

use crate::policy::blocks::{
    Block, BlockKind, BlockReadPlan, ExecutionContext, ExecutionError, MatchSelection,
    PropertyRead, TableSelection,
};
use crate::policy::ir::PropertyPath;
use crate::policy::queries::dependency::{DataModelPaths, EvalGraph, WriteScope};
use crate::policy::queries::path::PathClassifier;
use crate::policy::queries::scope::{EntitySources, ReferenceField};
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
        let start = Instant::now();

        self.validate_request(req)?;

        let order_to_run = self.compute_order_to_run(req)?;

        let store = req.input.depth_clone(1);
        let ref_targets: HashSet<Arc<str>> = self
            .reference_fields
            .iter()
            .map(|f| f.target.clone())
            .collect();
        let pool_index = RefPoolIndex::from_input(&store, ref_targets);
        store.hydrate_references(&self.reference_fields, &pool_index);

        let roots: Vec<Arc<str>> = if req.goals.is_empty() {
            self.eval_graph.terminal_sinks(&self.members)
        } else {
            req.goals.clone()
        };
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

    fn validate_request(&self, req: &EvaluateRequest) -> Result<(), EvaluationError> {
        for goal in &req.goals {
            if !self.eval_graph.contains(goal) {
                return Err(EvaluationError::GoalNotFound(goal.clone()));
            }
        }
        let validation_errors = self.input_schema.validate(&req.input);
        if !validation_errors.is_empty() {
            return Err(EvaluationError::InputValidationFailed {
                errors: validation_errors,
            });
        }
        Ok(())
    }

    fn compute_order_to_run(
        &self,
        req: &EvaluateRequest,
    ) -> Result<Vec<PropertyPath>, EvaluationError> {
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

        if req.goals.is_empty() {
            return Ok(visible_order);
        }

        let reachable = self.eval_graph.reachable_from(&req.goals);
        let mut missing: Vec<PropertyPath> = self
            .eval_graph
            .reachable_input_paths(&req.goals, visible)
            .into_iter()
            .filter(|p| {
                !self.data_model_paths.is_optional(p) && !self.input_satisfied(&req.input, p)
            })
            .collect();
        if !missing.is_empty() {
            missing.sort();
            return Err(EvaluationError::MissingRequiredInputs {
                goals: req.goals.clone(),
                missing,
            });
        }

        Ok(visible_order
            .iter()
            .filter(|p| reachable.contains(*p))
            .cloned()
            .collect())
    }

    fn input_satisfied(&self, input: &Variable, path: &str) -> bool {
        if Self::input_path_satisfied(input, path) {
            return true;
        }
        let Some((entity, rest)) = path.split_once('.') else {
            return false;
        };
        match self.entity_sources.get(entity) {
            Some(src) => {
                let resolved = format!("{}.{}", src.path, rest);
                Self::input_path_satisfied(input, &resolved)
            }
            None => false,
        }
    }

    fn input_path_satisfied(input: &Variable, path: &str) -> bool {
        let mut current = input.shallow_clone();
        for segment in path.split('.') {
            if current.as_array().is_some() {
                return true;
            }
            match current.dot(segment) {
                Some(v) => current = v,
                None => return false,
            }
        }
        true
    }
}

struct Driver<'a> {
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

enum Pick {
    Unconditional,
    Match(MatchSelection),
    Table(TableSelection),
}

impl Pick {
    fn collect_reads(&self, plan: &BlockReadPlan, out: &mut Vec<Arc<str>>) {
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
            (Some((name, _)), Some(fields)) => !fields.borrow().contains_key(name.as_str()),
            _ => false,
        };

        let (bound, slot) = if needs_owner {
            let wrapper = instance.depth_clone(1);
            let synthetic_owner = match (owner_binding, wrapper.as_object()) {
                (Some((name, owner_var)), Some(wrapper_fields)) => {
                    let key: Rc<str> = Rc::from(name.as_str());
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
            fields.remove("$");
            fields.insert(self.entity_key.clone(), bound);
        }
        Some(slot)
    }
}

enum InstanceSlot {
    Direct,
    Wrapped {
        wrapper: Variable,
        synthetic_owner: Option<(Rc<str>, Variable)>,
    },
}

impl InstanceSlot {
    fn write_back(&self, instance: &Variable) {
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
            if Self::is_injected_owner(synthetic_owner, key, value) {
                continue;
            }
            target.insert(key.clone(), value.shallow_clone());
        }
    }

    fn is_injected_owner(
        synthetic_owner: &Option<(Rc<str>, Variable)>,
        key: &Rc<str>,
        value: &Variable,
    ) -> bool {
        match synthetic_owner {
            Some((owner_key, injected)) if owner_key.as_ref() == key.as_ref() => {
                Self::same_ref(value, injected)
            }
            _ => false,
        }
    }

    fn same_ref(a: &Variable, b: &Variable) -> bool {
        match (a, b) {
            (Variable::Object(x), Variable::Object(y)) => Rc::ptr_eq(x, y),
            (Variable::Array(x), Variable::Array(y)) => Rc::ptr_eq(x, y),
            (Variable::String(x), Variable::String(y)) => Rc::ptr_eq(x, y),
            _ => a == b,
        }
    }
}

impl<'a> Driver<'a> {
    fn new(
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
            fields.borrow_mut().remove("$");
        }
        isolate
            .borrow_mut()
            .set_environment(self.env.shallow_clone());
    }

    fn demand(&mut self, prop: &str) -> Result<(), EvaluationError> {
        self.writers_of_longest_prefix(prop)
            .iter()
            .try_for_each(|owner| self.run_block(owner))
    }

    fn writers_of_longest_prefix(&self, prop: &str) -> &'a [BlockRef] {
        let graph = &self.artifact.eval_graph;
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

        let iterated = match rule.write_scope(&artifact.classifier) {
            WriteScope::Entity(entity) => artifact
                .entity_sources
                .get(entity.as_ref())
                .map(|src| (entity, src.path.clone(), src.owner.clone())),
            _ => None,
        };

        match iterated {
            Some((entity, path, src_owner)) => {
                self.run_iterated(owner, &rule, entity.as_ref(), &path, src_owner.as_deref())
            }
            None => self.run_singleton(owner, &rule),
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

    fn run_singleton(&mut self, owner: &BlockRef, rule: &Block) -> Result<(), EvaluationError> {
        let artifact = self.artifact;
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

        if matches!(rule.kind, BlockKind::Match(_) | BlockKind::DecisionTable(_)) {
            self.bind_env(&isolate);
        }
        let pick = Self::select_pick(rule, &ctx)?;
        let mut demanded: Vec<Arc<str>> = Vec::new();
        if let Some(plan) = artifact.read_plans.get(owner) {
            pick.collect_reads(plan, &mut demanded);
        }
        for path in &demanded {
            self.demand(path)?;
        }
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

    fn run_iterated(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        entity: &str,
        iter_path: &Arc<str>,
        owner_name: Option<&str>,
    ) -> Result<(), EvaluationError> {
        let Some(arr) = self
            .store
            .dot(iter_path.as_ref())
            .and_then(|v| v.as_array())
        else {
            return Ok(());
        };
        let instances: Vec<Variable> = arr.borrow().iter().map(|v| v.shallow_clone()).collect();
        let owner_binding = owner_name.and_then(|name| {
            let owner_path = iter_path.rsplit_once('.').map(|(o, _)| o)?;
            self.store
                .dot(owner_path)
                .map(|var| (name.to_string(), var))
        });
        let entity_key: Rc<str> = Rc::from(entity);
        let artifact = self.artifact;

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

        let mut demanded: Vec<Arc<str>> = Vec::new();
        if let Some(plan) = artifact.read_plans.get(owner) {
            for pick in &picks {
                pick.collect_reads(plan, &mut demanded);
            }
        }
        demanded.sort();
        demanded.dedup();
        for path in &demanded {
            self.demand(path)?;
        }

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
                    instance_path: Some(format!("{iter_path}.{idx}").into()),
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

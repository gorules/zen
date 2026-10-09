use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt};
use zen_expression::variable::Variable;
use zen_expression::IsolateError;
use zen_types::rccell::RcCell;
use zen_types::symbol::Symbol;

use crate::policy::blocks::{
    Block, BlockKind, BlockReadPlan, ExecutionError, MatchSelection, PropertyRead, TableSelection,
};
use crate::policy::evaluator::{EvalArtifact, Iteration};
use crate::workspace::types::{BlockExecution, BlockRef, BlockTrace, EvaluationError, WriteTrace};

pub(crate) struct Site<'a> {
    pub(crate) store: &'a Variable,
    pub(crate) policy_path: &'a Arc<str>,
    pub(crate) block_id: &'a Arc<str>,
    pub(crate) trace: bool,
    pub(crate) extras: bool,
    pub(crate) write_log: Option<&'a RefCell<Vec<WriteTrace>>>,
    pub(crate) env_mirror: Option<&'a Variable>,
}

impl Site<'_> {
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

pub(crate) trait Evaluate {
    fn environment(&mut self, env: Variable);
    fn select(&mut self, rule: &Block, site: &Site) -> Result<Pick, ExecutionError>;
    fn commit(&mut self, rule: &Block, site: &Site, pick: &Pick) -> Result<BlockTrace, ExecutionError>;
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

pub(crate) struct Iterated {
    entity: Rc<str>,
    iter_path: Arc<str>,
    instances: Vec<Variable>,
    single: bool,
    owner_binding: Option<(String, Variable)>,
    picks: Vec<Pick>,
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

pub(crate) struct HostState<'a> {
    artifact: &'a EvalArtifact,
    store: &'a Variable,
    env: Variable,
    entry: &'a Arc<str>,
    trace: bool,
    extras: bool,
    executions: Vec<BlockExecution>,
}

impl<'a> HostState<'a> {
    pub(crate) fn new(
        artifact: &'a EvalArtifact,
        store: &'a Variable,
        entry: &'a Arc<str>,
        trace: bool,
        extras: bool,
    ) -> Self {
        Self {
            artifact,
            store,
            env: store.depth_clone(1),
            entry,
            trace,
            extras,
            executions: Vec::new(),
        }
    }

    #[cfg(feature = "driver")]
    pub(crate) fn artifact(&self) -> &'a EvalArtifact {
        self.artifact
    }

    pub(crate) fn executions(self) -> Vec<BlockExecution> {
        self.executions
    }

    #[cfg(feature = "driver")]
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
        Self::write_into(self.store, Some(&self.env), path, value);
    }

    pub(crate) fn write_into(store: &Variable, env_mirror: Option<&Variable>, path: &str, value: Variable) {
        store.dot_insert(path, value);
        Self::mirror_top_level(store, env_mirror, path);
    }

    fn mirror_top_level(store: &Variable, env_mirror: Option<&Variable>, path: &str) {
        let Some(env) = env_mirror else {
            return;
        };
        let (Some(store_fields), Some(env_fields)) = (store.as_object(), env.as_object())
        else {
            return;
        };
        let segment = &path[..path.find('.').unwrap_or(path.len())];
        let store_fields = store_fields.borrow();
        let Some((key, value)) = store_fields.get_key_value(&Symbol::from(segment)) else {
            return;
        };
        env_fields
            .borrow_mut()
            .insert(key.clone(), value.shallow_clone());
    }

    fn bind_env(&self, eval: &mut impl Evaluate) {
        self.clear_dollar();
        eval.environment(self.env.shallow_clone());
    }

    pub(crate) fn iterated(&self, rule: &Block) -> Option<Iteration> {
        self.artifact.iteration(rule)
    }

    pub(crate) fn select(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        eval: &mut impl Evaluate,
    ) -> Result<Picked, EvaluationError> {
        match self.iterated(rule) {
            Some((entity, path, src_owner)) => {
                self.select_iterated(owner, rule, entity.as_ref(), &path, src_owner.as_deref(), eval)
            }
            None => self.select_singleton(owner, rule, eval).map(Picked::Singleton),
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
        eval: &mut impl Evaluate,
    ) -> Result<(), EvaluationError> {
        match picked {
            Picked::Skipped => Ok(()),
            Picked::Singleton(pick) => self.commit_singleton(owner, rule, pick, eval),
            Picked::Iterated(iterated) => self.commit_iterated(owner, rule, iterated, eval),
        }
    }

    fn site<'s>(
        &self,
        store: &'s Variable,
        owner: &'s BlockRef,
        rule: &'s Block,
        write_log: Option<&'s RefCell<Vec<WriteTrace>>>,
        env_mirror: Option<&'s Variable>,
    ) -> Site<'s> {
        Site {
            store,
            policy_path: &owner.policy_path,
            block_id: &rule.id,
            trace: self.trace,
            extras: self.extras,
            write_log,
            env_mirror,
        }
    }

    fn select_singleton(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        eval: &mut impl Evaluate,
    ) -> Result<Pick, EvaluationError> {
        let env = self.env.shallow_clone();
        let site = self.site(self.store, owner, rule, None, Some(&env));
        if matches!(rule.kind, BlockKind::Match(_) | BlockKind::DecisionTable(_)) {
            self.bind_env(eval);
        }
        Ok(eval.select(rule, &site)?)
    }

    fn commit_singleton(
        &mut self,
        owner: &BlockRef,
        rule: &Block,
        pick: Pick,
        eval: &mut impl Evaluate,
    ) -> Result<(), EvaluationError> {
        let write_log = (self.trace && self.extras).then(|| RefCell::new(Vec::new()));
        let env = self.env.shallow_clone();
        let bt = {
            let site = self.site(self.store, owner, rule, write_log.as_ref(), Some(&env));
            self.bind_env(eval);
            eval.commit(rule, &site, &pick)?
        };

        if self.trace {
            let trace_policy_path =
                (&owner.policy_path != self.entry).then(|| owner.policy_path.clone());
            let operand_values =
                Block::operand_values(self.extras, self.reads_for(owner), self.store);
            let reads = self.execution_reads(owner, &pick);
            self.executions.push(BlockExecution {
                block_id: rule.id.clone(),
                policy_path: trace_policy_path,
                instance_path: None,
                trace: bt,
                operand_values,
                writes: write_log.map(RefCell::into_inner).unwrap_or_default(),
                reads,
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
        eval: &mut impl Evaluate,
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
                eval.environment(phase.scoped.shallow_clone());
                let mut picks = Vec::with_capacity(instances.len());
                for instance in &instances {
                    let Some(_slot) = phase.bind(instance, &owner_binding) else {
                        picks.push(Pick::Unconditional);
                        continue;
                    };
                    let site = self.site(&phase.scoped, owner, rule, None, None);
                    picks.push(eval.select(rule, &site)?);
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
        eval: &mut impl Evaluate,
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
        eval.environment(phase.scoped.shallow_clone());
        for (idx, instance) in instances.iter().enumerate() {
            let Some(slot) = phase.bind(instance, &owner_binding) else {
                continue;
            };
            let write_log = (self.trace && self.extras).then(|| RefCell::new(Vec::new()));
            let result = {
                let site = self.site(&phase.scoped, owner, rule, write_log.as_ref(), None);
                eval.commit(rule, &site, &picks[idx])
            };
            let operand_values = match (&result, self.trace) {
                (Ok(_), true) => {
                    Block::operand_values(self.extras, self.reads_for(owner), &phase.scoped)
                }
                _ => HashMap::default(),
            };
            slot.write_back(instance);
            let bt = result?;
            if self.trace {
                let reads = self.execution_reads(owner, &picks[idx]);
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
                    reads,
                });
            }
        }
        Ok(())
    }

    fn reads_for(&self, owner: &BlockRef) -> &'a [PropertyRead] {
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

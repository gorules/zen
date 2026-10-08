use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use petgraph::algo::{tarjan_scc, toposort};
use petgraph::prelude::{NodeIndex, StableDiGraph};
use zen_expression::variable::VariableType;

use crate::policy::blocks::{
    AnalysisContext, AnalysisSummary, Block, InstanceSource, PropertyRead, SharedDeclaredPaths,
    SharedDictionaryTypes, SharedIntelliSense, SharedPoisonedPaths, TableCheck, WriteTarget,
};
use crate::policy::ir::{DataModelIr, ParsedPolicy, PropertyPath, PropertyTypeIr};
use crate::policy::queries::path::{PathClassifier, PathRoot};
use crate::policy::queries::scope::{EntityForm, EntityGraph, EntitySources, VariableTypeScope};
use crate::workspace::db::{AnalysisPass, PolicyDerivedCache, Snapshot};
use crate::workspace::types::{BlockRef, Diagnostic, DiagnosticCode, DiagnosticLocation};

#[derive(Debug)]
pub struct ShallowAnalyses {
    pub per_rule: Vec<RuleShallowAnalysis>,
    by_block: HashMap<BlockRef, usize>,
    rules_by_path: HashMap<Arc<str>, std::ops::Range<usize>>,
}

impl ShallowAnalyses {
    pub fn for_block(&self, block_ref: &BlockRef) -> Option<&RuleShallowAnalysis> {
        self.by_block
            .get(block_ref)
            .and_then(|&i| self.per_rule.get(i))
    }

    pub fn rules_for(&self, path: &Arc<str>) -> &[RuleShallowAnalysis] {
        self.rules_by_path
            .get(path)
            .map(|r| &self.per_rule[r.clone()])
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone)]
pub struct RuleShallowAnalysis {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    pub reads: Vec<PropertyRead>,
    pub writes: Vec<WriteTarget>,
}

impl RuleShallowAnalysis {
    pub fn is_in(&self, policy_path: &Arc<str>) -> bool {
        self.policy_path == *policy_path
    }
}

#[derive(Debug)]
pub struct EnrichedState {
    pub scope: VariableType,
    pub per_rule: Vec<RuleEnrichedAnalysis>,
    pub diagnostics: Vec<Diagnostic>,
    base_fields: HashMap<Rc<str>, VariableType>,
    owned: RefCell<Vec<VariableType>>,
    write_log: Vec<(PropertyPath, VariableType)>,
    own_writes: HashMap<BlockRef, std::ops::Range<usize>>,
    dependents: HashMap<BlockRef, HashSet<BlockRef>>,
    block_scopes: RefCell<HashMap<BlockRef, VariableType>>,
}

impl Drop for EnrichedState {
    fn drop(&mut self) {
        for root in self.owned.borrow().iter() {
            root.break_cycles();
        }
    }
}

impl EnrichedState {
    /// The request as declared by the data models, before any rule writes.
    pub(crate) fn declared_scope(&self) -> VariableType {
        VariableType::Object(Rc::new(RefCell::new(self.base_fields.clone())))
    }

    pub(crate) fn declared_at(&self, path: &str) -> VariableType {
        let (root, rest) = path.split_once('.').unwrap_or((path, ""));
        match self.base_fields.get(root) {
            Some(kind) if rest.is_empty() => kind.shallow_clone(),
            Some(kind) => kind.resolve_at(rest),
            None => VariableType::Null,
        }
    }

    pub(crate) fn scope_excluding(&self, block: &BlockRef) -> VariableType {
        if !self.own_writes.contains_key(block) {
            return self.scope.shallow_clone();
        }
        if let Some(cached) = self.block_scopes.borrow().get(block) {
            return cached.shallow_clone();
        }
        let hidden = self.dependents_closure(block);
        let scope = self.scope.isolated_clone();
        self.owned.borrow_mut().push(scope.shallow_clone());
        let hidden_ranges: Vec<&std::ops::Range<usize>> = hidden
            .iter()
            .filter_map(|b| self.own_writes.get(b))
            .collect();
        let mut visible: HashMap<&str, &VariableType> = HashMap::new();
        let mut hidden_paths: Vec<&str> = Vec::new();
        for (i, (path, resolved_type)) in self.write_log.iter().enumerate() {
            if hidden_ranges.iter().any(|r| r.contains(&i)) {
                hidden_paths.push(path);
            } else {
                visible.insert(path, resolved_type);
            }
        }
        for path in hidden_paths {
            match visible.get(path) {
                Some(resolved_type) => {
                    scope.insert_at_path(path, &resolved_type.isolated_clone(), true);
                }
                None => match self.declared_at(path) {
                    VariableType::Null | VariableType::Any => scope.remove_at_path(path),
                    declared => {
                        scope.insert_at_path(path, &declared.isolated_clone(), true);
                    }
                },
            }
        }
        self.block_scopes
            .borrow_mut()
            .insert(block.clone(), scope.shallow_clone());
        scope
    }

    fn dependents_closure(&self, block: &BlockRef) -> HashSet<BlockRef> {
        let mut seen: HashSet<BlockRef> = HashSet::new();
        let mut stack = vec![block.clone()];
        while let Some(current) = stack.pop() {
            if let Some(next) = self.dependents.get(&current) {
                stack.extend(next.iter().filter(|b| !seen.contains(*b)).cloned());
            }
            seen.insert(current);
        }
        seen
    }
}

#[derive(Debug, Clone)]
pub struct RuleEnrichedAnalysis {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    pub diagnostics: Vec<Diagnostic>,
    pub table_checks: Vec<TableCheck>,
}

#[derive(Debug)]
pub struct DependencyGraph {
    pub graph: StableDiGraph<PropertyNode, ()>,
    pub node_map: HashMap<PropertyPath, NodeIndex>,
}

impl DependencyGraph {
    pub(crate) fn link_instance_reads(
        &mut self,
        per_rule: &[&RuleShallowAnalysis],
        entity_graph: &EntityGraph,
        entity_sources: &EntitySources,
    ) {
        let entity_form = EntityForm::new(entity_sources);
        for rule in per_rule {
            let writes: Vec<NodeIndex> = rule
                .writes
                .iter()
                .filter_map(|w| self.node_map.get(&w.path).copied())
                .collect();
            for read in &rule.reads {
                let Some((prefix, entity)) = entity_graph.instance_form(&read.path, &entity_form)
                else {
                    continue;
                };
                let Some(entity_idx) = self
                    .node_map
                    .get(entity.as_str())
                    .copied()
                    .filter(|&idx| self.graph[idx].written_by.is_some())
                else {
                    continue;
                };
                let list = self.node_map.get(prefix).copied();
                for &target in writes.iter().chain(list.as_ref()) {
                    if entity_idx != target && !self.graph.contains_edge(entity_idx, target) {
                        self.graph.add_edge(entity_idx, target, ());
                    }
                }
            }
        }
    }

    fn block_dependents(&self) -> HashMap<BlockRef, HashSet<BlockRef>> {
        let mut out: HashMap<BlockRef, HashSet<BlockRef>> = HashMap::new();
        for edge in self.graph.edge_indices() {
            let Some((from, to)) = self.graph.edge_endpoints(edge) else {
                continue;
            };
            let (Some(writer), Some(reader)) = (
                self.graph[from].written_by.as_ref(),
                self.graph[to].written_by.as_ref(),
            ) else {
                continue;
            };
            if writer != reader
                && !PathPrefix::extends(&self.graph[to].path, &self.graph[from].path)
            {
                out.entry(writer.clone())
                    .or_default()
                    .insert(reader.clone());
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct PropertyNode {
    pub path: PropertyPath,
    pub resolved_type: VariableType,
    pub written_by: Option<BlockRef>,
    pub instance_source: Option<InstanceSource>,
}

impl PropertyNode {
    pub fn is_computed(&self) -> bool {
        self.written_by.is_some()
    }

    pub fn resolved_type_in(&self, scope: &VariableType, path: &str) -> VariableType {
        match scope.resolve_at(path) {
            VariableType::Any => self.resolved_type.to_acyclic(),
            t => t.to_acyclic(),
        }
    }
}

impl DependencyGraph {
    pub fn writer_for(&self, path: &str) -> Option<&BlockRef> {
        let idx = *self.node_map.get(path)?;
        self.graph[idx].written_by.as_ref()
    }

    pub fn computed_in<'a>(
        &'a self,
        visible: &'a HashSet<Arc<str>>,
    ) -> impl Iterator<Item = (&'a Arc<str>, &'a BlockRef, &'a PropertyNode)> + 'a {
        self.node_map.iter().filter_map(move |(path, &idx)| {
            let node = &self.graph[idx];
            let owner = node.written_by.as_ref()?;
            if !visible.contains(&owner.policy_path) || self.has_computed_ancestor(path) {
                return None;
            }
            Some((path, owner, node))
        })
    }

    fn has_computed_ancestor(&self, path: &str) -> bool {
        let mut cut = 0;
        while let Some(dot) = path[cut..].find('.') {
            let prefix = &path[..cut + dot];
            cut += dot + 1;
            if self.writer_for(prefix).is_some() {
                return true;
            }
        }
        false
    }

    pub fn reachable_from(&self, goals: &[Arc<str>]) -> HashSet<Arc<str>> {
        use petgraph::Incoming;
        let mut reachable: HashSet<Arc<str>> = HashSet::default();
        let mut stack: Vec<_> = goals
            .iter()
            .filter_map(|g| self.node_map.get(g).copied())
            .collect();
        while let Some(idx) = stack.pop() {
            let node = &self.graph[idx];
            if !reachable.insert(node.path.clone()) {
                continue;
            }
            for up in self.graph.neighbors_directed(idx, Incoming) {
                stack.push(up);
            }
        }
        reachable
    }

    pub fn cyclic_paths(&self) -> HashSet<Arc<str>> {
        let mut out: HashSet<Arc<str>> = HashSet::new();
        for scc in tarjan_scc(&self.graph) {
            let is_cycle = scc.len() > 1
                || scc
                    .first()
                    .is_some_and(|&idx| self.graph.contains_edge(idx, idx));
            if !is_cycle {
                continue;
            }
            for idx in scc {
                out.insert(self.graph[idx].path.clone());
            }
        }
        out
    }
}

pub struct EvalGraph {
    graph: StableDiGraph<PropertyPath, ()>,
    node_map: HashMap<PropertyPath, NodeIndex>,
    writers: HashMap<PropertyPath, BlockRef>,
    demand_writers: HashMap<PropertyPath, Vec<BlockRef>>,
}

impl EvalGraph {
    pub fn from_graph(dep: &DependencyGraph) -> Self {
        let mut graph = StableDiGraph::new();
        let mut node_map = HashMap::default();
        let mut writers = HashMap::default();
        let mut remap: HashMap<NodeIndex, NodeIndex> = HashMap::default();

        for (path, &old_idx) in &dep.node_map {
            let new_idx = graph.add_node(path.clone());
            node_map.insert(path.clone(), new_idx);
            remap.insert(old_idx, new_idx);
            if let Some(owner) = &dep.graph[old_idx].written_by {
                writers.insert(path.clone(), owner.clone());
            }
        }

        for edge in dep.graph.edge_indices() {
            if let Some((from, to)) = dep.graph.edge_endpoints(edge) {
                if let (Some(&from), Some(&to)) = (remap.get(&from), remap.get(&to)) {
                    graph.add_edge(from, to, ());
                }
            }
        }

        let demand_writers = Self::collect_demand_writers(&writers);

        Self {
            graph,
            node_map,
            writers,
            demand_writers,
        }
    }

    fn collect_demand_writers(
        writers: &HashMap<PropertyPath, BlockRef>,
    ) -> HashMap<PropertyPath, Vec<BlockRef>> {
        let mut out: HashMap<PropertyPath, Vec<BlockRef>> = HashMap::default();
        let mut sorted: Vec<(&PropertyPath, &BlockRef)> = writers.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (path, owner) in sorted {
            let mut push = |target: &PropertyPath| {
                let list = out.entry(target.clone()).or_default();
                if !list.contains(owner) {
                    list.push(owner.clone());
                }
            };
            push(path);
            let raw = path.as_ref();
            let mut cut = 0;
            while let Some(dot) = raw[cut..].find('.') {
                let prefix = &raw[..cut + dot];
                cut += dot + 1;
                if let Some((ancestor, _)) = writers.get_key_value(prefix) {
                    push(ancestor);
                }
            }
        }
        out
    }

    pub fn writer_for(&self, path: &str) -> Option<&BlockRef> {
        self.writers.get(path)
    }

    pub fn demand_writers_for(&self, path: &str) -> &[BlockRef] {
        self.demand_writers
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn contains(&self, path: &str) -> bool {
        self.node_map.contains_key(path)
    }

    pub fn reachable_from(&self, goals: &[Arc<str>]) -> HashSet<Arc<str>> {
        use petgraph::Incoming;
        let mut reachable: HashSet<Arc<str>> = HashSet::default();
        let mut stack: Vec<NodeIndex> = goals
            .iter()
            .filter_map(|g| self.node_map.get(g).copied())
            .collect();
        while let Some(idx) = stack.pop() {
            if !reachable.insert(self.graph[idx].clone()) {
                continue;
            }
            for up in self.graph.neighbors_directed(idx, Incoming) {
                stack.push(up);
            }
        }
        reachable
    }

    pub fn reachable_input_paths(
        &self,
        goals: &[Arc<str>],
        visible: &HashSet<Arc<str>>,
    ) -> HashSet<Arc<str>> {
        self.reachable_from(goals)
            .into_iter()
            .filter(|p| !self.written_at(p, visible))
            .collect()
    }

    pub(crate) fn written_at(&self, path: &str, visible: &HashSet<Arc<str>>) -> bool {
        let written = |path: &str| {
            self.writers
                .get(path)
                .is_some_and(|owner| visible.contains(&owner.policy_path))
        };
        written(path)
            || path
                .match_indices('.')
                .any(|(index, _)| written(&path[..index]))
    }

    pub fn terminal_sinks(&self, visible: &HashSet<Arc<str>>) -> Vec<Arc<str>> {
        use petgraph::Outgoing;
        let mut sinks: Vec<Arc<str>> = self
            .node_map
            .iter()
            .filter(|(path, _)| {
                self.writers
                    .get(path.as_ref())
                    .is_some_and(|owner| visible.contains(&owner.policy_path))
            })
            .filter(|(_, &idx)| {
                self.graph
                    .neighbors_directed(idx, Outgoing)
                    .next()
                    .is_none()
            })
            .map(|(path, _)| path.clone())
            .collect();
        sinks.sort();
        sinks
    }
}

impl Snapshot {
    #[allow(clippy::too_many_arguments)]
    fn analyze_block(
        rule: &Block,
        policy_path: &Arc<str>,
        rule_scope: VariableType,
        pass: AnalysisPass,
        intellisense: &SharedIntelliSense,
        dictionary_types: &SharedDictionaryTypes,
        poisoned_paths: &SharedPoisonedPaths,
        declared_paths: &SharedDeclaredPaths,
    ) -> AnalysisSummary {
        let mut ctx = AnalysisContext::new(
            rule_scope,
            policy_path.clone(),
            rule.id.clone(),
            intellisense.clone(),
            pass,
            dictionary_types.clone(),
            poisoned_paths.clone(),
            declared_paths.clone(),
        );
        rule.kind.analyze(&mut ctx);
        ctx.finish()
    }

    pub(crate) fn compute_shallow(
        base_scope: &VariableType,
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
        intellisense: &SharedIntelliSense,
        cache: &PolicyDerivedCache,
    ) -> ShallowAnalyses {
        let mut per_rule: Vec<RuleShallowAnalysis> = Vec::new();
        let mut rules_by_path: HashMap<Arc<str>, std::ops::Range<usize>> = HashMap::new();

        let mut sorted_paths: Vec<&Arc<str>> = all_parsed.keys().collect();
        sorted_paths.sort();
        for path in sorted_paths {
            let p = &all_parsed[path];
            let rules_start = per_rule.len();

            let no_dictionaries: SharedDictionaryTypes = Rc::new(ahash::HashMap::default());
            let no_poison: SharedPoisonedPaths = Default::default();
            let no_declared: SharedDeclaredPaths = Default::default();
            let policy_shallow = cache.shallow_or_compute(path, p, || {
                p.policy
                    .rules()
                    .map(|rule| {
                        let summary = Self::analyze_block(
                            rule,
                            path,
                            base_scope.shallow_clone(),
                            AnalysisPass::Shallow,
                            intellisense,
                            &no_dictionaries,
                            &no_poison,
                            &no_declared,
                        );
                        RuleShallowAnalysis {
                            policy_path: path.clone(),
                            block_id: rule.id.clone(),
                            reads: summary.reads,
                            writes: summary.writes,
                        }
                    })
                    .collect()
            });
            per_rule.extend(policy_shallow.iter().cloned());
            rules_by_path.insert(path.clone(), rules_start..per_rule.len());
        }

        let by_block = per_rule
            .iter()
            .enumerate()
            .map(|(i, r)| {
                (
                    BlockRef {
                        policy_path: r.policy_path.clone(),
                        block_id: r.block_id.clone(),
                    },
                    i,
                )
            })
            .collect();

        ShallowAnalyses {
            per_rule,
            by_block,
            rules_by_path,
        }
    }

    pub(crate) fn compute_graph(
        per_rule: &[&RuleShallowAnalysis],
        data_model_paths: &DataModelPaths,
        entity_sources: &crate::policy::queries::scope::EntitySources,
    ) -> DependencyGraph {
        let mut graph = StableDiGraph::new();
        let mut node_map: HashMap<PropertyPath, NodeIndex> = HashMap::new();
        let mut writers: HashMap<PropertyPath, (Arc<str>, Arc<str>)> = HashMap::new();

        let entity_form_map = EntityForm::new(entity_sources);
        let entity_form = |path: &str| -> Option<String> { entity_form_map.rewrite(path) };

        for &rule in per_rule {
            for read in &rule.reads {
                node_map.entry(read.path.clone()).or_insert_with(|| {
                    graph.add_node(PropertyNode {
                        path: read.path.clone(),
                        resolved_type: VariableType::Any,
                        written_by: None,
                        instance_source: None,
                    })
                });
            }

            for write in &rule.writes {
                if data_model_paths.matches_prefix(&write.path).is_some() {
                    continue;
                }

                let idx = *node_map.entry(write.path.clone()).or_insert_with(|| {
                    graph.add_node(PropertyNode {
                        path: write.path.clone(),
                        resolved_type: write.resolved_type.shallow_clone(),
                        written_by: None,
                        instance_source: None,
                    })
                });

                if !writers.contains_key(&write.path) {
                    writers.insert(
                        write.path.clone(),
                        (rule.policy_path.clone(), rule.block_id.clone()),
                    );
                    let node = &mut graph[idx];
                    node.resolved_type = write.resolved_type.shallow_clone();
                    node.written_by = Some(BlockRef {
                        policy_path: rule.policy_path.clone(),
                        block_id: rule.block_id.clone(),
                    });
                    node.instance_source = write.instance_source.clone();
                }

                let path = write.path.as_ref();
                let mut cut = 0;
                while let Some(dot) = path[cut..].find('.') {
                    let prefix = &path[..cut + dot];
                    cut += dot + 1;
                    if data_model_paths.matches_prefix(prefix).is_some() {
                        continue;
                    }
                    let prefix_path: PropertyPath = Arc::from(prefix);
                    let anc_idx = *node_map.entry(prefix_path.clone()).or_insert_with(|| {
                        graph.add_node(PropertyNode {
                            path: prefix_path.clone(),
                            resolved_type: VariableType::Any,
                            written_by: None,
                            instance_source: None,
                        })
                    });
                    if !writers.contains_key(&prefix_path) {
                        writers.insert(
                            prefix_path.clone(),
                            (rule.policy_path.clone(), rule.block_id.clone()),
                        );
                        graph[anc_idx].written_by = Some(BlockRef {
                            policy_path: rule.policy_path.clone(),
                            block_id: rule.block_id.clone(),
                        });
                    }
                    if idx != anc_idx {
                        graph.add_edge(idx, anc_idx, ());
                    }
                }
            }
        }

        for &rule in per_rule {
            for write in &rule.writes {
                if data_model_paths.matches_prefix(&write.path).is_some() {
                    continue;
                }
                let Some(&write_idx) = node_map.get(&write.path) else {
                    continue;
                };
                for read in &rule.reads {
                    if let Some(&read_idx) = node_map.get(&read.path) {
                        let reads_own_parent = PathPrefix::extends(&read.path, &write.path);
                        if read_idx != write_idx && !reads_own_parent {
                            graph.add_edge(read_idx, write_idx, ());
                        }
                    }
                    if let Some(entity_path) = entity_form(&read.path) {
                        if let Some(&entity_idx) = node_map.get(entity_path.as_str()) {
                            if entity_idx != write_idx {
                                graph.add_edge(entity_idx, write_idx, ());
                            }
                        }
                    }

                    let read_path = read.path.as_ref();
                    let mut cut = 0;
                    while let Some(dot) = read_path[cut..].find('.') {
                        let ancestor = &read_path[..cut + dot];
                        cut += dot + 1;
                        if let Some(&ancestor_idx) = node_map.get(ancestor) {
                            if ancestor_idx != write_idx
                                && graph[ancestor_idx].written_by.is_some()
                                && !PathPrefix::extends(ancestor, &write.path)
                            {
                                graph.add_edge(ancestor_idx, write_idx, ());
                            }
                        }
                    }
                }
            }
        }

        DependencyGraph { graph, node_map }
    }

    pub(crate) fn compute_execution_order(graph: &DependencyGraph) -> Vec<PropertyPath> {
        if let Ok(order) = toposort(&graph.graph, None) {
            return order
                .into_iter()
                .filter(|idx| graph.graph[*idx].written_by.is_some())
                .map(|idx| graph.graph[idx].path.clone())
                .collect();
        }
        let mut out: Vec<PropertyPath> = Vec::new();
        for scc in tarjan_scc(&graph.graph).into_iter().rev() {
            let mut paths: Vec<PropertyPath> = scc
                .into_iter()
                .filter(|idx| graph.graph[*idx].is_computed())
                .map(|idx| graph.graph[idx].path.clone())
                .collect();
            paths.sort();
            out.extend(paths);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compute_enriched(
        base_scope: &VariableType,
        scope_roots: Rc<RefCell<Vec<VariableType>>>,
        graph: &DependencyGraph,
        order: &[PropertyPath],
        rule_by_ref: &HashMap<BlockRef, Arc<Block>>,
        shallow: &ShallowAnalyses,
        members: &HashSet<Arc<str>>,
        intellisense: &SharedIntelliSense,
        dictionary_types: SharedDictionaryTypes,
        declared_paths: SharedDeclaredPaths,
    ) -> EnrichedState {
        let scope = base_scope.isolated_clone();
        scope_roots.borrow_mut().push(scope.shallow_clone());
        let base_fields = match base_scope {
            VariableType::Object(obj) => obj.borrow().clone(),
            _ => HashMap::new(),
        };
        let mut write_log: Vec<(PropertyPath, VariableType)> = Vec::new();
        let mut owned: Vec<VariableType> = Vec::new();
        let mut own_writes: HashMap<BlockRef, std::ops::Range<usize>> = HashMap::new();
        let mut per_rule: Vec<RuleEnrichedAnalysis> = Vec::new();
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        let writer_of: HashMap<&str, &BlockRef> = graph
            .graph
            .node_indices()
            .filter_map(|idx| {
                let node = &graph.graph[idx];
                node.written_by.as_ref().map(|o| (node.path.as_ref(), o))
            })
            .collect();

        let mut analyzed: HashSet<BlockRef> = HashSet::new();
        let mut schedule: Vec<(BlockRef, bool)> = Vec::new();
        for prop_path in order.iter() {
            if let Some(owner) = writer_of.get(prop_path.as_ref()) {
                if analyzed.insert((*owner).clone()) {
                    schedule.push(((*owner).clone(), true));
                }
            }
        }
        let mut remaining: Vec<BlockRef> = Vec::new();
        for member in members {
            for s in shallow.rules_for(member) {
                let key = BlockRef {
                    policy_path: s.policy_path.clone(),
                    block_id: s.block_id.clone(),
                };
                if analyzed.insert(key.clone()) {
                    remaining.push(key);
                }
            }
        }
        remaining.sort_by(|a, b| {
            a.policy_path
                .cmp(&b.policy_path)
                .then_with(|| a.block_id.cmp(&b.block_id))
        });
        schedule.extend(remaining.into_iter().map(|key| (key, false)));

        let poisoned_paths: SharedPoisonedPaths = Default::default();
        for (key, splice) in schedule {
            let Some(rule) = rule_by_ref.get(&key) else {
                continue;
            };
            let policy_path = &key.policy_path;
            let start = write_log.len();
            let summary = Self::analyze_block(
                rule,
                policy_path,
                scope.shallow_clone(),
                AnalysisPass::Enriched,
                intellisense,
                &dictionary_types,
                &poisoned_paths,
                &declared_paths,
            );
            for tw in &summary.writes {
                if declared_paths.matches_prefix(&tw.path).is_none() {
                    let frozen = tw.resolved_type.isolated_clone();
                    owned.push(frozen.shallow_clone());
                    write_log.push((tw.path.clone(), frozen));
                }
            }
            own_writes.insert(key.clone(), start..write_log.len());

            if splice {
                for tw in &summary.writes {
                    if declared_paths.matches_prefix(&tw.path).is_some() {
                        continue;
                    }
                    if !scope.insert_at_path(&tw.path, &tw.resolved_type, true) {
                        diagnostics.push(Diagnostic::error(
                            DiagnosticCode::InvalidWritePath,
                            DiagnosticLocation::block(policy_path.clone(), rule.id.clone())
                                .maybe_target(rule.kind.write_target(&tw.path)),
                            format!(
                                "cannot write to '{}': parent path is not an object",
                                tw.path
                            ),
                        ));
                    }
                }
            }

            per_rule.push(RuleEnrichedAnalysis {
                policy_path: policy_path.clone(),
                block_id: key.block_id.clone(),
                diagnostics: summary.diagnostics,
                table_checks: summary.table_checks,
            });
        }

        EnrichedState {
            scope,
            per_rule,
            diagnostics,
            base_fields,
            owned: RefCell::new(owned),
            write_log,
            own_writes,
            dependents: graph.block_dependents(),
            block_scopes: RefCell::new(HashMap::new()),
        }
    }
}

pub(crate) struct PathPrefix;

impl PathPrefix {
    pub(crate) fn extends(prefix: &str, path: &str) -> bool {
        prefix == path
            || (path.len() > prefix.len()
                && path.starts_with(prefix)
                && path.as_bytes()[prefix.len()] == b'.')
    }
}

#[derive(Clone, Default)]
pub struct DataModelPaths {
    all: HashSet<PropertyPath>,
    optional: HashSet<PropertyPath>,
    targets: HashMap<PropertyPath, Arc<str>>,
}

impl DataModelPaths {
    pub(crate) fn from_models<'a>(models: impl IntoIterator<Item = &'a DataModelIr>) -> Self {
        let mut all = HashSet::default();
        let mut optional = HashSet::default();
        let mut targets = HashMap::default();
        for dm in models {
            let is_global = dm.scope.is_global();
            for prop in &dm.properties {
                let path: PropertyPath = if is_global {
                    Arc::from(prop.name.as_ref())
                } else {
                    Arc::from(format!("{}.{}", dm.name, prop.name))
                };
                if prop.optional {
                    optional.insert(path.clone());
                }
                if let PropertyTypeIr::Relationship { target }
                | PropertyTypeIr::Reference { target } = &prop.kind
                {
                    targets.insert(path.clone(), target.clone());
                }
                all.insert(path);
            }
        }
        Self {
            all,
            optional,
            targets,
        }
    }

    pub(crate) fn declares(&self, path: &str) -> bool {
        self.all.contains(path)
    }

    pub fn matches_prefix(&self, write_path: &str) -> Option<&PropertyPath> {
        if let Some(p) = self.all.get(write_path) {
            return Some(p);
        }
        self.all
            .iter()
            .find(|p| PathPrefix::extends(p, write_path) || PathPrefix::extends(write_path, p))
    }

    pub(crate) fn optional_steps(&self, path: &str) -> Vec<bool> {
        let mut owner: Option<Arc<str>> = None;
        path.split('.')
            .enumerate()
            .map(|(i, segment)| {
                let key = match &owner {
                    Some(owner) => self.all.get(format!("{owner}.{segment}").as_str()),
                    None if i == 0 && !self.all.contains(segment) => {
                        owner = Some(Arc::from(segment));
                        return false;
                    }
                    None if i == 0 => self.all.get(segment),
                    None => None,
                };
                owner = key.and_then(|k| self.targets.get(k).cloned());
                key.is_some_and(|k| self.optional.contains(k))
            })
            .collect()
    }
}

impl Snapshot {
    pub(crate) fn compute_data_model_paths(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> DataModelPaths {
        DataModelPaths::from_models(
            all_parsed
                .values()
                .flat_map(|p| p.policy.data_models())
                .map(|(_, dm)| dm),
        )
    }
}

#[derive(Debug, Clone)]
pub enum WriteScope {
    Entity(Arc<str>),
    Global,
    Empty,
    Mixed,
}

impl Block {
    pub(crate) fn check_single_entity_scope(
        &self,
        policy_path: &Arc<str>,
        classifier: &PathClassifier,
        out: &mut Vec<Diagnostic>,
    ) {
        if !matches!(self.write_scope(classifier), WriteScope::Mixed) {
            return;
        }
        let labels = self.write_bucket_labels(classifier);
        out.push(Diagnostic::error(
            DiagnosticCode::MixedScope,
            DiagnosticLocation::block(policy_path.clone(), self.id.clone()),
            format!(
                "block writes to multiple scopes: {}. A block must be scoped to a single entity or to globals.",
                labels.join(", ")
            ),
        ));
    }

    pub(crate) fn write_scope(&self, classifier: &PathClassifier) -> WriteScope {
        let mut current: Option<WriteScope> = None;
        for path in self.write_paths() {
            if path.is_empty() {
                continue;
            }
            let next = match classifier.classify(&path) {
                PathRoot::Entity { entity, .. } => WriteScope::Entity(entity),
                PathRoot::Global { .. } => WriteScope::Global,
            };
            current = Some(match current {
                None => next,
                Some(prev) => prev.merge(next),
            });
        }
        current.unwrap_or(WriteScope::Empty)
    }

    fn write_bucket_labels(&self, classifier: &PathClassifier) -> Vec<String> {
        let mut entities: Vec<String> = Vec::new();
        let mut globals: Vec<String> = Vec::new();
        for path in self.write_paths() {
            if path.is_empty() {
                continue;
            }
            match classifier.classify(&path) {
                PathRoot::Entity { entity, .. } => {
                    let label = format!("entity '{entity}'");
                    if !entities.contains(&label) {
                        entities.push(label);
                    }
                }
                PathRoot::Global { name } => {
                    let label = format!("global '{name}'");
                    if !globals.contains(&label) {
                        globals.push(label);
                    }
                }
            }
        }
        entities.sort();
        globals.sort();
        entities.extend(globals);
        entities
    }

    pub(crate) fn write_paths(&self) -> Vec<Arc<str>> {
        self.kind.writes().into_iter().map(|w| w.path).collect()
    }
}

impl WriteScope {
    fn merge(self, other: WriteScope) -> WriteScope {
        match (self, other) {
            (WriteScope::Empty, x) | (x, WriteScope::Empty) => x,
            (WriteScope::Entity(a), WriteScope::Entity(b)) if a == b => WriteScope::Entity(a),
            (WriteScope::Global, WriteScope::Global) => WriteScope::Global,
            _ => WriteScope::Mixed,
        }
    }
}

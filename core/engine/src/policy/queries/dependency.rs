use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use petgraph::algo::{tarjan_scc, toposort};
use petgraph::prelude::{NodeIndex, StableDiGraph};
use zen_expression::variable::VariableType;

use crate::policy::blocks::{
    AnalysisContext, AnalysisSummary, Block, InstanceSource, PropertyRead, SharedDictionaryTypes,
    SharedIntelliSense, WriteTarget,
};
use crate::policy::ir::{DataModelIr, ParsedPolicy, PropertyPath};
use crate::policy::queries::path::{PathClassifier, PathRoot};
use crate::policy::queries::scope::{EntityForm, VariableTypeScope};
use crate::workspace::db::{AnalysisPass, PolicyDerivedCache, Snapshot};
use crate::workspace::types::{BlockRef, Diagnostic, DiagnosticCode, DiagnosticLocation};

#[derive(Debug)]
pub struct ShallowAnalyses {
    pub per_rule: Vec<RuleShallowAnalysis>,
    pub diagnostics: Vec<Diagnostic>,
    by_block: HashMap<BlockRef, usize>,
}

impl ShallowAnalyses {
    pub fn for_block(&self, block_ref: &BlockRef) -> Option<&RuleShallowAnalysis> {
        self.by_block
            .get(block_ref)
            .and_then(|&i| self.per_rule.get(i))
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
}

#[derive(Debug, Clone)]
pub struct RuleEnrichedAnalysis {
    pub policy_path: Arc<str>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug)]
pub struct DependencyGraph {
    pub graph: StableDiGraph<PropertyNode, ()>,
    pub node_map: HashMap<PropertyPath, NodeIndex>,
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
            .filter(|p| match self.writers.get(p.as_ref()) {
                None => true,
                Some(owner) => !visible.contains(&owner.policy_path),
            })
            .collect()
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
    fn analyze_block(
        rule: &Block,
        policy_path: &Arc<str>,
        rule_scope: VariableType,
        pass: AnalysisPass,
        intellisense: &SharedIntelliSense,
        dictionary_types: &SharedDictionaryTypes,
    ) -> AnalysisSummary {
        let mut ctx = AnalysisContext::new(
            rule_scope,
            policy_path.clone(),
            rule.id.clone(),
            intellisense.clone(),
            pass,
            dictionary_types.clone(),
        );
        rule.kind.analyze(&mut ctx);
        ctx.finish()
    }

    pub(crate) fn compute_shallow(
        base_scope: &VariableType,
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
        classifier: &PathClassifier,
        intellisense: &SharedIntelliSense,
        cache: &PolicyDerivedCache,
    ) -> ShallowAnalyses {
        let mut per_rule: Vec<RuleShallowAnalysis> = Vec::new();
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        let mut sorted_paths: Vec<&Arc<str>> = all_parsed.keys().collect();
        sorted_paths.sort();
        for path in sorted_paths {
            let p = &all_parsed[path];

            for rule in p.policy.rules() {
                rule.check_single_entity_scope(path, classifier, &mut diagnostics);
            }

            let no_dictionaries: SharedDictionaryTypes = Rc::new(ahash::HashMap::default());
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
            diagnostics,
            by_block,
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

    pub(crate) fn compute_enriched(
        base_scope: &VariableType,
        graph: &DependencyGraph,
        order: &[PropertyPath],
        rule_by_ref: &HashMap<BlockRef, Arc<Block>>,
        members: &HashSet<Arc<str>>,
        intellisense: &SharedIntelliSense,
        dictionary_types: SharedDictionaryTypes,
    ) -> EnrichedState {
        let scope = base_scope.shallow_clone();
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
        let mut remaining: Vec<&BlockRef> = rule_by_ref
            .keys()
            .filter(|key| members.contains(&key.policy_path) && !analyzed.contains(*key))
            .collect();
        remaining.sort_by(|a, b| {
            a.policy_path
                .cmp(&b.policy_path)
                .then_with(|| a.block_id.cmp(&b.block_id))
        });
        schedule.extend(remaining.into_iter().map(|key| (key.clone(), false)));

        for (key, splice) in schedule {
            let Some(rule) = rule_by_ref.get(&key) else {
                continue;
            };
            let policy_path = &key.policy_path;
            let summary = Self::analyze_block(
                rule,
                policy_path,
                scope.shallow_clone(),
                AnalysisPass::Enriched,
                intellisense,
                &dictionary_types,
            );

            if splice {
                for tw in &summary.writes {
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
                diagnostics: summary.diagnostics,
            });
        }

        EnrichedState {
            scope,
            per_rule,
            diagnostics,
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

#[derive(Clone)]
pub struct DataModelPaths {
    all: HashSet<PropertyPath>,
    optional: HashSet<PropertyPath>,
}

impl DataModelPaths {
    pub(crate) fn from_models<'a>(models: impl IntoIterator<Item = &'a DataModelIr>) -> Self {
        let mut all = HashSet::default();
        let mut optional = HashSet::default();
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
                all.insert(path);
            }
        }
        Self { all, optional }
    }

    pub fn matches_prefix(&self, write_path: &str) -> Option<&PropertyPath> {
        if let Some(p) = self.all.get(write_path) {
            return Some(p);
        }
        self.all
            .iter()
            .find(|p| PathPrefix::extends(p, write_path) || PathPrefix::extends(write_path, p))
    }

    pub fn is_optional(&self, path: &str) -> bool {
        self.optional.contains(path) || self.optional.iter().any(|p| PathPrefix::extends(p, path))
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

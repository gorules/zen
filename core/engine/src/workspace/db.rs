use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::intellisense::IntelliSense;
use zen_expression::variable::VariableType;
use zen_expression::{Isolate, OpcodeCache};

use crate::model::{DecisionContent, PolicyContent};
use crate::policy::blocks::{
    Block, BlockKind, BlockReadPlan, IntelliSenseSource, PropertyRead, ReadFlattener,
    SharedIntelliSense,
};
use crate::policy::evaluator::EvalArtifact;
use crate::policy::ir::{
    DataModelIr, DictionaryIr, ParsedPolicy, Policy, Property, PropertyPath, Scope,
};
use crate::policy::queries::dependency::{
    DataModelPaths, DependencyGraph, EnrichedState, EvalGraph, RuleShallowAnalysis, ShallowAnalyses,
};
use crate::policy::queries::path::PathClassifier;
use crate::policy::queries::scope::{
    DataModelEntry, EntityForm, EntityGraph, EntitySources, ImportGraph, ReferenceField,
    VariableTypeScope,
};
use crate::policy::raw::PolicyDocument;
use crate::workspace::graph::function::{
    FunctionKey, FunctionResolutionRequest, FunctionTypeResolver, ResolvedFunction,
};
use crate::workspace::graph::GraphAnalysis;
use crate::workspace::types::{BlockRef, Diagnostic, ExpressionKind, InstanceTarget};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AnalysisPass {
    Shallow,
    Enriched,
}

pub(crate) struct GraphDeps {
    docs: Vec<(Arc<str>, Option<Arc<DecisionContent>>)>,
    functions: Vec<(FunctionKey, u64)>,
}

#[derive(Default)]
pub(crate) struct PolicyDerivedCache {
    parsed: RefCell<HashMap<Arc<str>, (Arc<PolicyDocument>, Arc<ParsedPolicy>)>>,
    shallow: RefCell<HashMap<Arc<str>, (Arc<ParsedPolicy>, Arc<Vec<RuleShallowAnalysis>>)>>,
    units: RefCell<HashMap<Vec<Arc<str>>, (Vec<Arc<ParsedPolicy>>, Arc<Unit>)>>,
    graphs: RefCell<HashMap<Arc<str>, (GraphDeps, Arc<GraphAnalysis>)>>,
}

impl PolicyDerivedCache {
    fn retain(&self, live: &HashMap<Arc<str>, Arc<DecisionContent>>) {
        self.parsed
            .borrow_mut()
            .retain(|path, _| live.contains_key(path));
        self.shallow
            .borrow_mut()
            .retain(|path, _| live.contains_key(path));
        self.units
            .borrow_mut()
            .retain(|members, _| members.iter().all(|m| live.contains_key(m)));
        self.graphs
            .borrow_mut()
            .retain(|path, _| live.contains_key(path));
    }

    fn unit_or_compute(
        &self,
        members: &[Arc<str>],
        parsed: &[Arc<ParsedPolicy>],
        compute: impl FnOnce() -> Unit,
    ) -> Arc<Unit> {
        if let Some((cached_parsed, unit)) = self.units.borrow().get(members) {
            if cached_parsed.len() == parsed.len()
                && cached_parsed
                    .iter()
                    .zip(parsed)
                    .all(|(a, b)| Arc::ptr_eq(a, b))
            {
                return unit.clone();
            }
        }
        let unit = Arc::new(compute());
        self.units
            .borrow_mut()
            .insert(members.to_vec(), (parsed.to_vec(), unit.clone()));
        unit
    }

    fn parsed_or_compute(
        &self,
        path: &Arc<str>,
        doc: &Arc<PolicyDocument>,
        compute: impl FnOnce() -> Arc<ParsedPolicy>,
    ) -> Arc<ParsedPolicy> {
        if let Some((cached_doc, parsed)) = self.parsed.borrow().get(path) {
            if Arc::ptr_eq(cached_doc, doc) {
                return parsed.clone();
            }
        }
        let parsed = compute();
        self.parsed
            .borrow_mut()
            .insert(path.clone(), (doc.clone(), parsed.clone()));
        parsed
    }

    pub(crate) fn shallow_or_compute(
        &self,
        path: &Arc<str>,
        parsed: &Arc<ParsedPolicy>,
        compute: impl FnOnce() -> Vec<RuleShallowAnalysis>,
    ) -> Arc<Vec<RuleShallowAnalysis>> {
        if let Some((cached_parsed, shallow)) = self.shallow.borrow().get(path) {
            if Arc::ptr_eq(cached_parsed, parsed) {
                return shallow.clone();
            }
        }
        let shallow = Arc::new(compute());
        self.shallow
            .borrow_mut()
            .insert(path.clone(), (parsed.clone(), shallow.clone()));
        shallow
    }
}

struct Inputs {
    documents: HashMap<Arc<str>, Arc<DecisionContent>>,
}

pub struct Snapshot {
    pub(crate) base_scope: VariableType,
    pub(crate) all_parsed: Arc<HashMap<Arc<str>, Arc<ParsedPolicy>>>,
    pub(crate) graphs: HashMap<Arc<str>, Arc<DecisionContent>>,
    pub(crate) rule_by_ref: Arc<HashMap<BlockRef, Arc<Block>>>,
    pub(crate) import_graph: Arc<ImportGraph>,
    pub(crate) shallow: Arc<ShallowAnalyses>,
    pub(crate) components: Vec<Vec<Arc<str>>>,
    pub(crate) policy_to_component: HashMap<Arc<str>, usize>,
    pub(crate) units: RefCell<HashMap<usize, Arc<Unit>>>,
    pub(crate) policy_diagnostics: RefCell<HashMap<Arc<str>, Arc<Vec<Diagnostic>>>>,
    pub(crate) eval_artifacts: RefCell<HashMap<Arc<str>, Arc<EvalArtifact>>>,
    pub(crate) path_set: OnceCell<Arc<HashSet<Arc<str>>>>,
    pub(crate) import_cycles: OnceCell<Arc<HashMap<Arc<str>, Vec<Diagnostic>>>>,
}

pub struct Unit {
    pub members: HashSet<Arc<str>>,
    pub entity_sources: Arc<EntitySources>,
    pub entity_graph: EntityGraph,
    pub reference_fields: Vec<ReferenceField>,
    pub data_model_paths: DataModelPaths,
    pub classifier: PathClassifier,
    pub dep_graph: DependencyGraph,
    pub execution_order: Vec<PropertyPath>,
    pub computed_instances: HashMap<Arc<str>, InstanceTarget>,
    enriched_once: OnceCell<Arc<EnrichedState>>,
    opcode_cache: OnceCell<Arc<OpcodeCache>>,
    pub data_models: Vec<DataModelEntry>,
    pub entities: HashMap<Arc<str>, Arc<DataModelIr>>,
    pub dictionaries: HashMap<Arc<str>, Arc<DictionaryIr>>,
    pub dictionary_blocks: Vec<DictionaryUnitEntry>,
}

impl Unit {
    pub(crate) fn dictionary_types(&self) -> HashMap<Arc<str>, VariableType> {
        self.dictionaries
            .iter()
            .map(|(name, dict)| (name.clone(), dict.enum_type()))
            .collect()
    }
}

pub struct DictionaryUnitEntry {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    pub ir: Arc<DictionaryIr>,
}

pub struct Db {
    inputs: RefCell<Inputs>,
    snapshot: RefCell<Option<Arc<Snapshot>>>,
    cache: PolicyDerivedCache,
    intellisense: SharedIntelliSense,
    graph_intellisense: SharedIntelliSense,
    pub(crate) graph_stack: RefCell<Vec<Arc<str>>>,
    graph_dep_frames: RefCell<Vec<HashSet<Arc<str>>>>,
    graph_fn_frames: RefCell<Vec<HashMap<FunctionKey, u64>>>,
    function_types: RefCell<HashMap<FunctionKey, ResolvedFunction>>,
    function_requests: RefCell<Vec<FunctionResolutionRequest>>,
    function_requested: RefCell<HashSet<FunctionKey>>,
    function_resolver: RefCell<Option<Box<FunctionTypeResolver>>>,
    scope_roots: RefCell<Vec<VariableType>>,
}

impl Drop for Db {
    fn drop(&mut self) {
        for root in self.scope_roots.borrow().iter() {
            root.break_cycles();
        }
    }
}

impl Db {
    pub fn new() -> Self {
        Self {
            inputs: RefCell::new(Inputs {
                documents: HashMap::default(),
            }),
            snapshot: RefCell::new(None),
            cache: PolicyDerivedCache::default(),
            intellisense: Rc::new(RefCell::new(IntelliSense::new().with_strict(true))),
            graph_intellisense: Rc::new(RefCell::new(IntelliSense::new().with_strict(true))),
            graph_stack: RefCell::new(Vec::new()),
            graph_dep_frames: RefCell::new(Vec::new()),
            graph_fn_frames: RefCell::new(Vec::new()),
            function_types: RefCell::new(HashMap::default()),
            function_requests: RefCell::new(Vec::new()),
            function_requested: RefCell::new(HashSet::default()),
            function_resolver: RefCell::new(None),
            scope_roots: RefCell::new(Vec::new()),
        }
    }

    pub fn set_document(&mut self, path: Arc<str>, doc: Arc<DecisionContent>) {
        self.inputs.borrow_mut().documents.insert(path, doc);
        self.invalidate_snapshot();
    }

    pub fn set_policy(&mut self, path: Arc<str>, doc: Arc<PolicyDocument>) {
        self.set_document(path, Arc::new(DecisionContent::Policy(PolicyContent(doc))));
    }

    pub fn remove_document(&mut self, path: &str) -> bool {
        let existed = self.inputs.borrow_mut().documents.remove(path).is_some();
        if existed {
            self.invalidate_snapshot();
        }
        existed
    }

    pub fn document_paths(&self) -> Vec<Arc<str>> {
        self.inputs.borrow().documents.keys().cloned().collect()
    }

    pub(crate) fn path_set(&self) -> Arc<HashSet<Arc<str>>> {
        let snap = self.snapshot();
        snap.path_set
            .get_or_init(|| Arc::new(self.document_paths().into_iter().collect()))
            .clone()
    }

    pub(crate) fn import_cycles(&self) -> Arc<HashMap<Arc<str>, Vec<Diagnostic>>> {
        use crate::workspace::types::{DiagnosticCode, DiagnosticLocation};
        use petgraph::algo::tarjan_scc;

        let snap = self.snapshot();
        snap.import_cycles
            .get_or_init(|| {
                let import_graph = &snap.import_graph;
                let mut out: HashMap<Arc<str>, Vec<Diagnostic>> = HashMap::new();
                for scc in tarjan_scc(&import_graph.graph) {
                    let is_cycle = scc.len() > 1
                        || scc
                            .first()
                            .is_some_and(|&idx| import_graph.graph.contains_edge(idx, idx));
                    if !is_cycle {
                        continue;
                    }
                    let mut members: Vec<Arc<str>> = scc
                        .iter()
                        .map(|&idx| import_graph.graph[idx].clone())
                        .collect();
                    members.sort();
                    let rendered: Vec<String> = members.iter().map(|p| p.to_string()).collect();
                    let message = format!("circular import among: {}", rendered.join(", "));
                    for member in &members {
                        out.entry(member.clone())
                            .or_default()
                            .push(Diagnostic::error(
                                DiagnosticCode::CircularImport,
                                DiagnosticLocation::policy(member.clone()),
                                message.clone(),
                            ));
                    }
                }
                Arc::new(out)
            })
            .clone()
    }

    pub fn raw_document(&self, path: &str) -> Option<Arc<DecisionContent>> {
        self.inputs.borrow().documents.get(path).cloned()
    }

    pub fn raw_policy(&self, path: &str) -> Option<Arc<PolicyDocument>> {
        match self.raw_document(path)?.as_ref() {
            DecisionContent::Policy(policy) => Some(policy.0.clone()),
            DecisionContent::Graph(_) => None,
        }
    }

    pub fn intellisense(&self) -> SharedIntelliSense {
        self.intellisense.clone()
    }

    pub fn graph_intellisense(&self) -> SharedIntelliSense {
        self.graph_intellisense.clone()
    }

    pub(crate) fn invalidate_snapshot(&self) {
        *self.snapshot.borrow_mut() = None;
    }

    pub(crate) fn set_function_resolver(&self, resolver: Option<Box<FunctionTypeResolver>>) {
        *self.function_resolver.borrow_mut() = resolver;
        self.invalidate_snapshot();
    }

    pub(crate) fn function_types(&self) -> &RefCell<HashMap<FunctionKey, ResolvedFunction>> {
        &self.function_types
    }

    pub(crate) fn function_requests(&self) -> &RefCell<Vec<FunctionResolutionRequest>> {
        &self.function_requests
    }

    pub(crate) fn function_requested(&self) -> &RefCell<HashSet<FunctionKey>> {
        &self.function_requested
    }

    pub(crate) fn function_resolver(&self) -> &RefCell<Option<Box<FunctionTypeResolver>>> {
        &self.function_resolver
    }

    pub(crate) fn graph_fn_record(&self, key: FunctionKey, state: u64) {
        if let Some(frame) = self.graph_fn_frames.borrow_mut().last_mut() {
            frame.insert(key, state);
        }
    }

    pub(crate) fn graph_dep_record(&self, path: &Arc<str>) {
        if let Some(frame) = self.graph_dep_frames.borrow_mut().last_mut() {
            frame.insert(path.clone());
        }
    }

    pub(crate) fn graph_dep_record_many(&self, paths: impl IntoIterator<Item = Arc<str>>) {
        if let Some(frame) = self.graph_dep_frames.borrow_mut().last_mut() {
            frame.extend(paths);
        }
    }

    pub(crate) fn graph_dep_frame_push(&self, path: &Arc<str>) {
        let mut frame = HashSet::default();
        frame.insert(path.clone());
        self.graph_dep_frames.borrow_mut().push(frame);
        self.graph_fn_frames.borrow_mut().push(HashMap::default());
    }

    pub(crate) fn graph_dep_frame_pop(&self) -> (HashSet<Arc<str>>, HashMap<FunctionKey, u64>) {
        let docs = self.graph_dep_frames.borrow_mut().pop().unwrap_or_default();
        let functions = self.graph_fn_frames.borrow_mut().pop().unwrap_or_default();
        (docs, functions)
    }

    pub(crate) fn cached_graph_analysis(&self, path: &Arc<str>) -> Option<Arc<GraphAnalysis>> {
        let (dep_paths, fn_stamps, analysis) = {
            let cache = self.cache.graphs.borrow();
            let (deps, analysis) = cache.get(path)?;
            let inputs = self.inputs.borrow();
            let docs_valid = deps.docs.iter().all(|(dep_path, stamp)| match stamp {
                Some(stamp) => inputs
                    .documents
                    .get(dep_path)
                    .is_some_and(|doc| Arc::ptr_eq(doc, stamp)),
                None => !inputs.documents.contains_key(dep_path),
            });
            if !docs_valid {
                return None;
            }
            let functions_valid = deps
                .functions
                .iter()
                .all(|&(key, state)| self.function_state(key) == state);
            if !functions_valid {
                return None;
            }
            let dep_paths: Vec<Arc<str>> = deps.docs.iter().map(|(p, _)| p.clone()).collect();
            (dep_paths, deps.functions.clone(), analysis.clone())
        };
        self.graph_dep_record_many(dep_paths);
        for (key, state) in fn_stamps {
            self.graph_fn_record(key, state);
        }
        Some(analysis)
    }

    pub(crate) fn store_graph_analysis(
        &self,
        path: &Arc<str>,
        docs: HashSet<Arc<str>>,
        functions: HashMap<FunctionKey, u64>,
        analysis: Arc<GraphAnalysis>,
    ) {
        let deps = {
            let inputs = self.inputs.borrow();
            let mut sorted: Vec<Arc<str>> = docs.into_iter().collect();
            sorted.sort();
            let docs = sorted
                .into_iter()
                .map(|p| {
                    let stamp = inputs.documents.get(&p).cloned();
                    (p, stamp)
                })
                .collect();
            let mut functions: Vec<(FunctionKey, u64)> = functions.into_iter().collect();
            functions.sort_unstable();
            GraphDeps { docs, functions }
        };
        self.cache
            .graphs
            .borrow_mut()
            .insert(path.clone(), (deps, analysis));
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        if let Some(s) = self.snapshot.borrow().clone() {
            return s;
        }
        let s = Arc::new(Snapshot::compute(
            &self.inputs.borrow().documents,
            &self.intellisense,
            &self.cache,
        ));
        self.scope_roots
            .borrow_mut()
            .push(s.base_scope.shallow_clone());
        *self.snapshot.borrow_mut() = Some(s.clone());
        s
    }

    pub fn parsed(&self, path: &Arc<str>) -> Option<Arc<ParsedPolicy>> {
        self.snapshot().all_parsed.get(path).cloned()
    }

    pub fn unit(&self, policy: &str) -> Arc<Unit> {
        let snap = self.snapshot();
        let Some(&idx) = snap.policy_to_component.get(policy) else {
            return self.cache.unit_or_compute(&[], &[], || {
                Snapshot::compute_unit(&[], &snap.all_parsed, &snap.shallow)
            });
        };
        if let Some(u) = snap.units.borrow().get(&idx).cloned() {
            return u;
        }
        let members = &snap.components[idx];
        let parsed: Vec<Arc<ParsedPolicy>> = members
            .iter()
            .filter_map(|m| snap.all_parsed.get(m).cloned())
            .collect();
        let unit = self.cache.unit_or_compute(members, &parsed, || {
            Snapshot::compute_unit(members, &snap.all_parsed, &snap.shallow)
        });
        snap.units.borrow_mut().insert(idx, unit.clone());
        unit
    }

    pub(crate) fn unit_for_property(&self, property: &str) -> Arc<Unit> {
        let snap = self.snapshot();
        let policy = Self::policy_touching(&snap.shallow, property)
            .or_else(|| Self::policy_writing_longest_prefix(&snap.shallow, property))
            .or_else(|| snap.components.first().and_then(|c| c.first()).cloned());
        match policy {
            Some(p) => self.unit(&p),
            None => self.unit(""),
        }
    }

    fn policy_touching(shallow: &ShallowAnalyses, property: &str) -> Option<Arc<str>> {
        shallow.per_rule.iter().find_map(|r| {
            let touches = r.writes.iter().any(|w| w.path.as_ref() == property)
                || r.reads.iter().any(|rd| rd.path.as_ref() == property);
            touches.then(|| r.policy_path.clone())
        })
    }

    fn policy_writing_longest_prefix(
        shallow: &ShallowAnalyses,
        property: &str,
    ) -> Option<Arc<str>> {
        let mut end = property.len();
        while let Some(dot) = property[..end].rfind('.') {
            let prefix = &property[..dot];
            let writer = shallow.per_rule.iter().find_map(|r| {
                r.writes
                    .iter()
                    .any(|w| w.path.as_ref() == prefix)
                    .then(|| r.policy_path.clone())
            });
            if writer.is_some() {
                return writer;
            }
            end = dot;
        }
        None
    }

    pub(crate) fn enriched(&self, policy: &str) -> Arc<EnrichedState> {
        let unit = self.unit(policy);
        self.enriched_of_unit(&unit)
    }

    pub(crate) fn enriched_of_unit(&self, unit: &Unit) -> Arc<EnrichedState> {
        unit.enriched_once
            .get_or_init(|| {
                let snap = self.snapshot();
                let subset: HashMap<Arc<str>, Arc<ParsedPolicy>> = unit
                    .members
                    .iter()
                    .filter_map(|m| snap.all_parsed.get_key_value(m))
                    .map(|(p, v)| (p.clone(), v.clone()))
                    .collect();
                let base_scope = Snapshot::compute_base_scope(&subset, &unit.entity_sources);
                self.scope_roots
                    .borrow_mut()
                    .push(base_scope.shallow_clone());
                Arc::new(Snapshot::compute_enriched(
                    &base_scope,
                    &unit.dep_graph,
                    &unit.execution_order,
                    &snap.rule_by_ref,
                    &snap.shallow,
                    &unit.members,
                    &self.intellisense,
                    Rc::new(unit.dictionary_types()),
                ))
            })
            .clone()
    }

    pub(crate) fn opcode_cache_of_unit(&self, unit: &Unit) -> Arc<OpcodeCache> {
        unit.opcode_cache
            .get_or_init(|| {
                let snap = self.snapshot();
                let mut sources: Vec<(Arc<str>, ExpressionKind)> = Vec::new();
                for member in &unit.members {
                    let Some(parsed) = snap.all_parsed.get(member) else {
                        continue;
                    };
                    for rule in parsed.policy.rules() {
                        for loc in rule.kind.expressions(&rule.id) {
                            sources.push((loc.source, loc.kind));
                        }
                        if let BlockKind::DecisionTable(dt) = &rule.kind {
                            for col in &dt.inputs {
                                if let Some(field) =
                                    col.field.as_ref().filter(|f| !f.as_ref().is_empty())
                                {
                                    sources.push((field.clone(), ExpressionKind::Standard));
                                }
                            }
                        }
                    }
                }

                let mut cache = OpcodeCache::new();
                let mut isolate = Isolate::new();
                for (source, kind) in &sources {
                    let map = match kind {
                        ExpressionKind::Standard => &mut cache.standard,
                        ExpressionKind::Unary => &mut cache.unary,
                    };
                    if map.contains_key(source) {
                        continue;
                    }
                    let bytecode = match kind {
                        ExpressionKind::Standard => isolate
                            .compile_standard(source)
                            .map(|e| e.bytecode().to_vec()),
                        ExpressionKind::Unary => {
                            isolate.compile_unary(source).map(|e| e.bytecode().to_vec())
                        }
                    };
                    if let Ok(bc) = bytecode {
                        map.insert(source.clone(), Arc::from(bc));
                    }
                }
                Arc::new(cache)
            })
            .clone()
    }

    pub(crate) fn eval_artifact(&self, policy: &str) -> Arc<EvalArtifact> {
        let snap = self.snapshot();
        if let Some(artifact) = snap.eval_artifacts.borrow().get(policy).cloned() {
            return artifact;
        }

        let unit = self.unit(policy);
        let opcode_cache = self.opcode_cache_of_unit(&unit);
        let input_schema = self.input_schema(policy);
        let eval_graph = EvalGraph::from_graph(&unit.dep_graph);
        let reads: HashMap<BlockRef, Arc<[PropertyRead]>> = unit
            .members
            .iter()
            .flat_map(|m| snap.shallow.rules_for(m))
            .map(|s| {
                (
                    BlockRef {
                        policy_path: s.policy_path.clone(),
                        block_id: s.block_id.clone(),
                    },
                    Arc::from(s.reads.clone()),
                )
            })
            .collect();

        let intellisense = self.intellisense();
        let entity_form = EntityForm::new(unit.entity_sources.as_ref());
        let unit_refs: Vec<(&BlockRef, &Arc<Block>)> = unit
            .members
            .iter()
            .flat_map(|m| snap.shallow.rules_for(m))
            .filter_map(|s| {
                let block_ref = BlockRef {
                    policy_path: s.policy_path.clone(),
                    block_id: s.block_id.clone(),
                };
                snap.rule_by_ref
                    .get_key_value(&block_ref)
                    .map(|(r, b)| (r, b))
            })
            .collect();
        let read_plans: HashMap<BlockRef, BlockReadPlan> = unit_refs
            .into_iter()
            .map(|(r, block)| {
                let mut flatten = |src: &Arc<str>, kind: ExpressionKind| -> Vec<Arc<str>> {
                    if src.is_empty() {
                        return Vec::new();
                    }
                    let analysis =
                        IntelliSenseSource::reads_only(&mut intellisense.borrow_mut(), src, kind);
                    let mut out = Vec::new();
                    ReadFlattener::extend_from_deps(&analysis.reads, &None, &mut out);
                    out.into_iter()
                        .filter_map(|rd| {
                            if rd.unresolved {
                                None
                            } else if rd.via_alias {
                                entity_form
                                    .rewrite(rd.path.as_ref())
                                    .map(Arc::from)
                                    .or(Some(rd.path))
                            } else {
                                Some(rd.path)
                            }
                        })
                        .collect()
                };
                (r.clone(), block.kind.read_plan(&mut flatten))
            })
            .collect();

        let artifact = Arc::new(EvalArtifact {
            members: unit.members.clone(),
            eval_graph,
            execution_order: unit.execution_order.clone(),
            entity_sources: unit.entity_sources.clone(),
            reference_fields: unit.reference_fields.clone(),
            data_model_paths: unit.data_model_paths.clone(),
            classifier: unit.classifier.clone(),
            opcode_cache,
            rule_by_ref: snap.rule_by_ref.clone(),
            input_schema,
            reads,
            read_plans,
        });
        snap.eval_artifacts
            .borrow_mut()
            .insert(Arc::from(policy), artifact.clone());
        artifact
    }

    pub fn rule_by_ref(&self) -> Arc<HashMap<BlockRef, Arc<Block>>> {
        self.snapshot().rule_by_ref.clone()
    }

    pub fn block_ir(&self, block_ref: &BlockRef) -> Option<Arc<Block>> {
        self.snapshot().rule_by_ref.get(block_ref).cloned()
    }

    pub fn block_doc(&self, block_ref: &BlockRef) -> Option<crate::policy::raw::BlockDoc> {
        let policy = self.raw_policy(&block_ref.policy_path)?;
        policy
            .blocks
            .iter()
            .find(|b| b.id() == Some(block_ref.block_id.as_ref()))
            .cloned()
    }

    pub fn shallow(&self) -> Arc<ShallowAnalyses> {
        self.snapshot().shallow.clone()
    }

    pub fn policy_diagnostics(&self, path: &Arc<str>) -> Arc<Vec<Diagnostic>> {
        let snap = self.snapshot();
        if let Some(d) = snap.policy_diagnostics.borrow().get(path).cloned() {
            return d;
        }
        let value = if snap.graphs.contains_key(path) {
            Arc::new(
                self.graph_analysis(path)
                    .map(|analysis| analysis.diagnostics.clone())
                    .unwrap_or_default(),
            )
        } else {
            Arc::new(self.compute_policy_diagnostics(path))
        };
        snap.policy_diagnostics
            .borrow_mut()
            .insert(path.clone(), value.clone());
        value
    }

    pub fn all_diagnostics(&self) -> Vec<Diagnostic> {
        let mut paths = self.document_paths();
        paths.sort();
        paths
            .iter()
            .flat_map(|p| (*self.policy_diagnostics(p)).clone())
            .collect()
    }
}

impl Default for Db {
    fn default() -> Self {
        Self::new()
    }
}

impl Snapshot {
    fn compute(
        documents: &HashMap<Arc<str>, Arc<DecisionContent>>,
        intellisense: &SharedIntelliSense,
        cache: &PolicyDerivedCache,
    ) -> Snapshot {
        let mut policies: HashMap<Arc<str>, Arc<PolicyDocument>> = HashMap::default();
        let mut graphs: HashMap<Arc<str>, Arc<DecisionContent>> = HashMap::default();
        for (path, doc) in documents {
            match doc.as_ref() {
                DecisionContent::Policy(policy) => {
                    policies.insert(path.clone(), policy.0.clone());
                }
                DecisionContent::Graph(_) => {
                    graphs.insert(path.clone(), doc.clone());
                }
            }
        }
        cache.retain(documents);
        let policies = &policies;
        let all_parsed = Arc::new(Self::parse_all(policies, cache));
        let rule_by_ref = Arc::new(Self::build_rule_by_ref(&all_parsed));
        let import_graph = Arc::new(Self::compute_import_graph(&all_parsed));

        let entity_sources = Self::compute_entity_sources(&all_parsed);
        let base_scope = Self::compute_base_scope(&all_parsed, &entity_sources);
        let classifier = Self::compute_path_classifier(&all_parsed);
        let shallow = Arc::new(Self::compute_shallow(
            &base_scope,
            &all_parsed,
            &classifier,
            intellisense,
            cache,
        ));

        let (components, policy_to_component) =
            Self::compute_components(&import_graph, &all_parsed);

        Snapshot {
            base_scope,
            all_parsed,
            graphs,
            rule_by_ref,
            import_graph,
            shallow,
            components,
            policy_to_component,
            units: RefCell::new(HashMap::new()),
            policy_diagnostics: RefCell::new(HashMap::new()),
            eval_artifacts: RefCell::new(HashMap::new()),
            path_set: OnceCell::new(),
            import_cycles: OnceCell::new(),
        }
    }

    fn compute_components(
        import_graph: &ImportGraph,
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> (Vec<Vec<Arc<str>>>, HashMap<Arc<str>, usize>) {
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        let mut components: Vec<Vec<Arc<str>>> = Vec::new();
        let mut sorted: Vec<&Arc<str>> = all_parsed.keys().collect();
        sorted.sort();
        for path in sorted {
            if seen.contains(path) {
                continue;
            }
            let mut members: Vec<Arc<str>> = Vec::new();
            let mut stack = vec![path.clone()];
            while let Some(p) = stack.pop() {
                if !seen.insert(p.clone()) {
                    continue;
                }
                members.push(p.clone());
                if let Some(&idx) = import_graph.node_map.get(&p) {
                    for n in import_graph.graph.neighbors_undirected(idx) {
                        stack.push(import_graph.graph[n].clone());
                    }
                }
            }
            members.sort();
            components.push(members);
        }
        let mut policy_to_component: HashMap<Arc<str>, usize> = HashMap::new();
        for (idx, members) in components.iter().enumerate() {
            for m in members {
                policy_to_component.insert(m.clone(), idx);
            }
        }
        (components, policy_to_component)
    }

    fn compute_unit(
        members: &[Arc<str>],
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
        shallow: &ShallowAnalyses,
    ) -> Unit {
        let member_set: HashSet<Arc<str>> = members.iter().cloned().collect();
        let subset: HashMap<Arc<str>, Arc<ParsedPolicy>> = members
            .iter()
            .filter_map(|m| all_parsed.get_key_value(m))
            .map(|(p, v)| (p.clone(), v.clone()))
            .collect();

        let entity_sources = Arc::new(Self::compute_entity_sources(&subset));
        let mut entity_graph = Self::compute_entity_graph(&subset, &entity_sources);
        let reference_fields = Self::compute_reference_fields(&subset);
        let data_model_paths = Self::compute_data_model_paths(&subset);
        let classifier = Self::compute_path_classifier(&subset);

        let mut sorted_members: Vec<&Arc<str>> = members.iter().collect();
        sorted_members.sort();
        let per_rule: Vec<&RuleShallowAnalysis> = sorted_members
            .iter()
            .flat_map(|m| shallow.rules_for(m))
            .collect();
        let dep_graph = Self::compute_graph(&per_rule, &data_model_paths, &entity_sources);
        let execution_order = Self::compute_execution_order(&dep_graph);

        let (_, pool_roots) = DataModelIr::classify_roots(
            subset
                .values()
                .flat_map(|p| p.policy.data_models().map(|(_, dm)| dm)),
        );
        let computed_instances = entity_graph.resolve_instance_targets(&dep_graph, &pool_roots);
        entity_graph.register_computed(&computed_instances);

        let mut data_models: Vec<DataModelEntry> = subset
            .iter()
            .flat_map(|(path, p)| {
                p.policy
                    .data_models
                    .iter()
                    .map(move |block| DataModelEntry {
                        policy_path: path.clone(),
                        block_id: block.id.clone(),
                        ir: block.ir.clone(),
                    })
            })
            .collect();
        data_models.sort_by(|a, b| {
            a.ir.name
                .cmp(&b.ir.name)
                .then_with(|| a.policy_path.cmp(&b.policy_path))
        });

        let entities = Self::compute_unit_entities(&subset);
        let dictionaries = Self::compute_dictionary_map(&subset);

        let mut dictionary_blocks: Vec<DictionaryUnitEntry> = subset
            .iter()
            .flat_map(|(path, p)| {
                p.policy
                    .dictionaries
                    .iter()
                    .map(move |block| DictionaryUnitEntry {
                        policy_path: path.clone(),
                        block_id: block.id.clone(),
                        ir: block.ir.clone(),
                    })
            })
            .collect();
        dictionary_blocks.sort_by(|a, b| {
            a.ir.name
                .cmp(&b.ir.name)
                .then_with(|| a.policy_path.cmp(&b.policy_path))
        });

        Unit {
            members: member_set,
            entity_sources,
            entity_graph,
            reference_fields,
            data_model_paths,
            classifier,
            dep_graph,
            execution_order,
            computed_instances,
            enriched_once: OnceCell::new(),
            opcode_cache: OnceCell::new(),
            data_models,
            entities,
            dictionaries,
            dictionary_blocks,
        }
    }

    fn compute_unit_entities(
        subset: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> HashMap<Arc<str>, Arc<DataModelIr>> {
        let mut sorted: Vec<&Arc<str>> = subset.keys().collect();
        sorted.sort();
        let mut props_by_entity: HashMap<Arc<str>, Vec<Property>> = HashMap::new();
        for pp in sorted {
            for (_, dm) in subset[pp].policy.entity_data_models() {
                let bucket = props_by_entity.entry(dm.name.clone()).or_default();
                for prop in &dm.properties {
                    if !bucket.iter().any(|p| p.name == prop.name) {
                        bucket.push(prop.clone());
                    }
                }
            }
        }
        props_by_entity
            .into_iter()
            .map(|(name, properties)| {
                let dm = Arc::new(DataModelIr {
                    name: name.clone(),
                    scope: Scope::Entity,
                    properties,
                });
                (name, dm)
            })
            .collect()
    }

    fn parse_all(
        policies: &HashMap<Arc<str>, Arc<PolicyDocument>>,
        cache: &PolicyDerivedCache,
    ) -> HashMap<Arc<str>, Arc<ParsedPolicy>> {
        policies
            .iter()
            .map(|(path, doc)| {
                let parsed =
                    cache.parsed_or_compute(path, doc, || Arc::new(Policy::parse(path, doc)));
                (path.clone(), parsed)
            })
            .collect()
    }

    fn build_rule_by_ref(
        all_parsed: &HashMap<Arc<str>, Arc<ParsedPolicy>>,
    ) -> HashMap<BlockRef, Arc<Block>> {
        all_parsed
            .iter()
            .flat_map(|(path, p)| {
                p.policy.rules().map(move |rule| {
                    (
                        BlockRef {
                            policy_path: path.clone(),
                            block_id: rule.id.clone(),
                        },
                        Arc::new(rule.clone()),
                    )
                })
            })
            .collect()
    }
}

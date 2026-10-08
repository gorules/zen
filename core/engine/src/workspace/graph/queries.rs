use std::collections::VecDeque;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::VariableType;

use crate::policy::ir::DataModelIr;
use crate::workspace::db::{Db, DictionaryUnitEntry};
use crate::workspace::graph::analysis::{
    GraphAnalysis, GraphAnalyzer, GraphSignature, SignatureResolution,
};
use crate::workspace::reads::ReadView;

use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::types::{InputProperty, OutputProperty, PropertyKind, ScopeRequest, SuppliedBy};

impl Db {
    pub(crate) fn graph_analysis(&self, path: &Arc<str>) -> Option<Arc<GraphAnalysis>> {
        let snap = self.snapshot();
        let doc = snap.graphs.get(path)?.clone();
        if let Some(analysis) = self.cached_graph_analysis(path) {
            return Some(analysis);
        }
        let content = doc.as_graph()?;
        self.graph_stack.borrow_mut().push(path.clone());
        self.graph_dep_frame_push(path);
        let analysis = Arc::new(GraphAnalyzer::new(self, path.clone(), content).analyze());
        self.graph_stack.borrow_mut().pop();
        let (frame, functions) = self.graph_dep_frame_pop();
        for (&key, &state) in &functions {
            self.graph_fn_record(key, state);
        }
        self.store_graph_analysis(path, frame, functions, analysis.clone());
        Some(analysis)
    }

    pub(crate) fn is_graph(&self, path: &str) -> bool {
        self.snapshot().graphs.contains_key(path)
    }

    pub(crate) fn graph_imports(&self, path: &str) -> Vec<Arc<str>> {
        self.snapshot()
            .graphs
            .get(path)
            .and_then(|doc| doc.as_graph())
            .map(|content| content.imports.clone())
            .unwrap_or_default()
    }

    pub(crate) fn graph_dictionary_blocks(&self, imports: &[Arc<str>]) -> Vec<DictionaryUnitEntry> {
        let snap = self.snapshot();
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        let mut visited: HashSet<Arc<str>> = HashSet::default();
        let mut queue: VecDeque<Arc<str>> = imports.iter().cloned().collect();
        let mut out: Vec<DictionaryUnitEntry> = Vec::new();
        while let Some(path) = queue.pop_front() {
            if !visited.insert(path.clone()) {
                continue;
            }
            let Some(parsed) = snap.all_parsed.get(&path) else {
                continue;
            };
            for block in &parsed.policy.dictionaries {
                if !seen.insert(block.ir.name.clone()) {
                    continue;
                }
                out.push(DictionaryUnitEntry {
                    policy_path: path.clone(),
                    block_id: block.id.clone(),
                    ir: block.ir.clone(),
                });
            }
            queue.extend(parsed.policy.imports().iter().cloned());
        }
        out
    }

    /// The entities a graph's imports make visible (the imports and what
    /// they import), each with the policy that defines it; the first of a
    /// name wins.
    pub(crate) fn graph_entity_blocks(&self, imports: &[Arc<str>]) -> Vec<EntityUnitEntry> {
        let snap = self.snapshot();
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        let mut visited: HashSet<Arc<str>> = HashSet::default();
        let mut queue: VecDeque<Arc<str>> = imports.iter().cloned().collect();
        let mut out: Vec<EntityUnitEntry> = Vec::new();
        while let Some(path) = queue.pop_front() {
            if !visited.insert(path.clone()) {
                continue;
            }
            let Some(parsed) = snap.all_parsed.get(&path) else {
                continue;
            };
            for block in &parsed.policy.data_models {
                if block.ir.scope.is_global() || block.ir.name.is_empty() {
                    continue;
                }
                if !seen.insert(block.ir.name.clone()) {
                    continue;
                }
                out.push(EntityUnitEntry {
                    policy_path: path.clone(),
                    block_id: block.id.clone(),
                    ir: block.ir.clone(),
                });
            }
            queue.extend(parsed.policy.imports().iter().cloned());
        }
        out
    }

    pub(crate) fn graph_dictionary_types(
        &self,
        imports: &[Arc<str>],
    ) -> HashMap<Arc<str>, VariableType> {
        let mut out = HashMap::new();
        let view = ReadView::Dictionaries(self.dictionary_view(imports));
        for import in imports {
            self.graph_dep_record_view(import, view.clone());
        }
        for entry in self.graph_dictionary_blocks(imports) {
            out.insert(entry.ir.name.clone(), entry.ir.enum_type());
        }
        out
    }

    pub(crate) fn decision_signature(&self, key: &str) -> SignatureResolution {
        let key_arc: Arc<str> = Arc::from(key);
        let resolution = self.resolve_signature(&key_arc);
        self.graph_dep_record_view(&key_arc, ReadView::Signature(resolution.detached()));
        resolution
    }

    fn resolve_signature(&self, key_arc: &Arc<str>) -> SignatureResolution {
        let key: &str = key_arc;
        let snap = self.snapshot();
        if snap.graphs.contains_key(key_arc) {
            if self.graph_stack.borrow().iter().any(|p| p.as_ref() == key) {
                return SignatureResolution::Recursive;
            }
            return match self.graph_analysis(key_arc) {
                Some(analysis) => SignatureResolution::Found(analysis.signature.clone()),
                None => SignatureResolution::Missing,
            };
        }
        if snap.all_parsed.contains_key(key_arc) {
            let req = ScopeRequest::for_policy(key);
            let input = VariableType::empty_object();
            let output = VariableType::empty_object();
            for property in self.inputs(&req) {
                input.insert_at_path(&property.path, &property.resolved_type, true);
                output.insert_at_path(&property.path, &property.resolved_type, true);
            }
            for property in self.outputs(&req) {
                output.insert_at_path(&property.path, &property.resolved_type, true);
            }
            return SignatureResolution::Found(GraphSignature { input, output });
        }
        SignatureResolution::Missing
    }

    pub(crate) fn graph_inputs(&self, path: &str) -> Vec<InputProperty> {
        let path_arc: Arc<str> = Arc::from(path);
        if let Some(properties) = self.graph_entity_inputs(&path_arc) {
            return properties;
        }
        let Some(analysis) = self.graph_analysis(&path_arc) else {
            return Vec::new();
        };
        match &analysis.signature.input {
            VariableType::Object(fields) => {
                let mut properties: Vec<InputProperty> = fields
                    .borrow()
                    .iter()
                    .map(|(key, resolved_type)| {
                        InputProperty::request(Arc::from(key.as_ref()), resolved_type.shallow_clone())
                    })
                    .collect();
                properties.sort_by(|a, b| a.path.cmp(&b.path));
                properties
            }
            VariableType::Any => analysis
                .inferred_inputs
                .iter()
                .map(|path| InputProperty::request(path.clone(), VariableType::Any))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A request typed by an entity, as the caller sends it: the entity's
    /// fields, a reference as its id; what the host supplies marked so.
    fn graph_entity_inputs(&self, path: &Arc<str>) -> Option<Vec<InputProperty>> {
        let snap = self.snapshot();
        let content = snap.graphs.get(path).cloned()?;
        let content = content.as_graph()?;
        let target = content.request_target()?;
        let entities: HashMap<Arc<str>, Arc<DataModelIr>> = self
            .graph_entity_blocks(&content.imports)
            .into_iter()
            .map(|b| (b.ir.name.clone(), b.ir))
            .collect();
        let dictionaries: HashMap<Arc<str>, Arc<crate::policy::ir::DictionaryIr>> = self
            .graph_dictionary_blocks(&content.imports)
            .into_iter()
            .map(|b| (b.ir.name.clone(), b.ir))
            .collect();
        let entity = entities.get(&target)?;
        let mut properties: Vec<InputProperty> = entity
            .properties
            .iter()
            .map(|property| {
                let mut visited: HashSet<Arc<str>> = HashSet::default();
                InputProperty {
                    path: property.name.clone(),
                    resolved_type: DataModelIr::wire_property_type(
                        property,
                        &entities,
                        &dictionaries,
                        &mut visited,
                    ),
                    optional: property.optional || property.default.is_some(),
                    supplied_by: if property.supply.is_some() {
                        SuppliedBy::Host
                    } else {
                        SuppliedBy::Request
                    },
                    default: property.default.as_deref().cloned(),
                }
            })
            .collect();
        properties.sort_by(|a, b| a.path.cmp(&b.path));
        Some(properties)
    }

    pub(crate) fn graph_outputs(&self, path: &str) -> Vec<OutputProperty> {
        let path_arc: Arc<str> = Arc::from(path);
        let Some(analysis) = self.graph_analysis(&path_arc) else {
            return Vec::new();
        };
        let (output_base, _) = analysis.signature.output.unwrap_nullable();
        let VariableType::Object(fields) = output_base else {
            return Vec::new();
        };
        let input_has = |key: &str| -> bool {
            let (input_base, _) = analysis.signature.input.unwrap_nullable();
            match input_base {
                VariableType::Object(input_fields) => input_fields.borrow().contains_key(key),
                _ => false,
            }
        };
        let mut properties: Vec<OutputProperty> = fields
            .borrow()
            .iter()
            .filter_map(|(key, resolved_type)| {
                let written_by = self.graph_written_by(&path_arc, key.as_ref());
                if written_by.is_none() && input_has(key.as_ref()) {
                    return None;
                }
                Some(OutputProperty {
                    path: Arc::from(key.as_ref()),
                    resolved_type: resolved_type.shallow_clone(),
                    kind: PropertyKind::Computed,
                    written_by,
                    instance_of: None,
                })
            })
            .collect();
        properties.sort_by(|a, b| a.path.cmp(&b.path));
        properties
    }

    pub(crate) fn graph_unchecked_nodes(&self, path: &str) -> Vec<Arc<str>> {
        let path_arc: Arc<str> = Arc::from(path);
        let Some(analysis) = self.graph_analysis(&path_arc) else {
            return Vec::new();
        };
        let mut nodes: Vec<Arc<str>> = analysis
            .nodes
            .iter()
            .filter(|(_, node)| node.unchecked || node.opaque)
            .map(|(id, _)| id.clone())
            .collect();
        nodes.sort();
        nodes
    }
}

/// An entity a graph sees through its imports, and where it is defined.
#[derive(Clone)]
pub(crate) struct EntityUnitEntry {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    pub ir: Arc<DataModelIr>,
}

impl PartialEq for EntityUnitEntry {
    // The same parsed block: a policy's blocks are parsed again only when it changes.
    fn eq(&self, other: &Self) -> bool {
        self.policy_path == other.policy_path
            && self.block_id == other.block_id
            && Arc::ptr_eq(&self.ir, &other.ir)
    }
}

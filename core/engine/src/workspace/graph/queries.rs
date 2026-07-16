use std::collections::VecDeque;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::VariableType;

use crate::workspace::db::{Db, DictionaryUnitEntry};
use crate::workspace::graph::analysis::{
    GraphAnalysis, GraphAnalyzer, GraphSignature, SignatureResolution,
};
use zen_types::decision::DecisionNodeKind;

use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::types::{
    Cursor, CursorTarget, InputProperty, OutputProperty, PropertyKind, ScopeRequest,
};

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
        let (docs, functions) = self.graph_dep_frame_pop();
        self.graph_dep_record_many(docs.iter().cloned());
        for (&key, &state) in &functions {
            self.graph_fn_record(key, state);
        }
        self.store_graph_analysis(path, docs, functions, analysis.clone());
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
            self.graph_dep_record(&path);
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

    pub(crate) fn graph_dictionary_types(
        &self,
        imports: &[Arc<str>],
    ) -> HashMap<Arc<str>, VariableType> {
        let mut out = HashMap::new();
        for entry in self.graph_dictionary_blocks(imports) {
            out.insert(entry.ir.name.clone(), entry.ir.enum_type());
        }
        out
    }

    pub(crate) fn decision_signature(&self, key: &str) -> SignatureResolution {
        let key_arc: Arc<str> = Arc::from(key);
        self.graph_dep_record(&key_arc);
        let snap = self.snapshot();
        if snap.graphs.contains_key(&key_arc) {
            if self.graph_stack.borrow().iter().any(|p| p.as_ref() == key) {
                return SignatureResolution::Recursive;
            }
            return match self.graph_analysis(&key_arc) {
                Some(analysis) => SignatureResolution::Found(analysis.signature.clone()),
                None => SignatureResolution::Missing,
            };
        }
        if snap.all_parsed.contains_key(&key_arc) {
            if let Some(&component) = snap.policy_to_component.get(&key_arc) {
                self.graph_dep_record_many(snap.components[component].iter().cloned());
            }
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
        let Some(analysis) = self.graph_analysis(&path_arc) else {
            return Vec::new();
        };
        match &analysis.signature.input {
            VariableType::Object(fields) => {
                let mut properties: Vec<InputProperty> = fields
                    .borrow()
                    .iter()
                    .map(|(key, resolved_type)| InputProperty {
                        path: Arc::from(key.as_ref()),
                        resolved_type: resolved_type.shallow_clone(),
                    })
                    .collect();
                properties.sort_by(|a, b| a.path.cmp(&b.path));
                properties
            }
            VariableType::Any => analysis
                .inferred_inputs
                .iter()
                .map(|path| InputProperty {
                    path: path.clone(),
                    resolved_type: VariableType::Any,
                })
                .collect(),
            _ => Vec::new(),
        }
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

    pub(crate) fn graph_cell_expected(&self, cursor: &Cursor) -> Option<VariableType> {
        let snap = self.snapshot();
        let doc = snap.graphs.get(&cursor.policy_path)?.clone();
        let content = doc.as_graph()?;
        let node = content.nodes.iter().find(|n| n.id == cursor.block_id)?;
        let DecisionNodeKind::DecisionTableNode { content } = &node.kind else {
            return None;
        };
        let CursorTarget::DecisionTableCell { col, .. } = &cursor.target else {
            return None;
        };
        let dictionaries = self.graph_dictionary_types(&doc.as_graph()?.imports);
        GraphAnalyzer::output_expected(content, col, &dictionaries)
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

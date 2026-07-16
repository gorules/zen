use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_types::decision::{DecisionNode, DecisionNodeKind};

use crate::policy::blocks::{PropertyRead, ReadFlattener};
use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::db::Db;
use crate::workspace::graph::analysis::GraphAnalyzer;
use crate::workspace::graph::editor::{NodePaths, PathMatch, ReadBase};
use crate::workspace::types::{BlockRef, DependencyNode, ExpressionKind, ScopeRequest};
use zen_expression::variable::VariableType;

struct WriterHit {
    node_id: Arc<str>,
    reads: Vec<Arc<str>>,
    nested: Vec<DependencyNode>,
    unresolved: bool,
}

type DepKey = (Arc<str>, Arc<str>);

#[derive(Default)]
struct DepWalk {
    visited: HashSet<DepKey>,
    memo: HashMap<DepKey, DependencyNode>,
}

impl DepWalk {
    fn new() -> Self {
        Self {
            visited: HashSet::new(),
            memo: HashMap::new(),
        }
    }
}

impl Db {
    pub(crate) fn graph_dependencies(&self, document: &Arc<str>, target: &str) -> DependencyNode {
        self.graph_dep_node(document, target, &mut DepWalk::new())
    }

    pub(crate) fn graph_written_by(&self, document: &Arc<str>, target: &str) -> Option<BlockRef> {
        self.graph_written_by_guarded(document, target, &mut DepWalk::new())
    }

    fn graph_written_by_guarded(
        &self,
        document: &Arc<str>,
        target: &str,
        walk: &mut DepWalk,
    ) -> Option<BlockRef> {
        let key: DepKey = (document.clone(), Arc::from(target));
        if !walk.visited.insert(key.clone()) {
            return None;
        }
        let hits = self.graph_writer_hits(document, target, walk, false);
        walk.visited.remove(&key);
        hits.first().map(|hit| BlockRef {
            policy_path: document.clone(),
            block_id: hit.node_id.clone(),
        })
    }

    fn graph_dep_node(
        &self,
        document: &Arc<str>,
        target: &str,
        walk: &mut DepWalk,
    ) -> DependencyNode {
        let key: DepKey = (document.clone(), Arc::from(target));
        let property = key.1.clone();
        if let Some(cached) = walk.memo.get(&key) {
            return cached.clone();
        }
        let resolved_type = self.graph_resolved_type(document, target);
        if !walk.visited.insert(key.clone()) {
            return DependencyNode {
                property,
                written_by: self.graph_written_by(document, target),
                unresolved: false,
                resolved_type,
                deps: Vec::new(),
            };
        }
        let hits = self.graph_writer_hits(document, target, walk, true);
        let node = if let Some(first) = hits.first() {
            let written_by = BlockRef {
                policy_path: document.clone(),
                block_id: first.node_id.clone(),
            };
            let unresolved = hits.iter().any(|hit| hit.unresolved);

            let mut seen: HashSet<Arc<str>> = HashSet::new();
            let mut deps: Vec<DependencyNode> = Vec::new();
            for hit in &hits {
                for read in &hit.reads {
                    if read.as_ref() == target || !seen.insert(read.clone()) {
                        continue;
                    }
                    deps.push(self.graph_dep_node(document, read, walk));
                }
            }
            for hit in hits {
                for nested in hit.nested {
                    if seen.insert(nested.property.clone()) {
                        deps.push(nested);
                    }
                }
            }

            DependencyNode {
                property,
                written_by: Some(written_by),
                unresolved,
                resolved_type,
                deps,
            }
        } else {
            DependencyNode {
                property,
                written_by: None,
                unresolved: false,
                resolved_type,
                deps: Vec::new(),
            }
        };
        walk.visited.remove(&key);
        walk.memo.insert(key, node.clone());
        node
    }

    fn graph_resolved_type(&self, document: &Arc<str>, target: &str) -> VariableType {
        let Some(analysis) = self.graph_analysis(document) else {
            return VariableType::Any;
        };
        let from_output = analysis.signature.output.resolve_at(target);
        if !matches!(from_output, VariableType::Any) {
            return from_output.to_acyclic();
        }
        analysis.signature.input.resolve_at(target).to_acyclic()
    }

    fn graph_writer_hits(
        &self,
        document: &Arc<str>,
        target: &str,
        walk: &mut DepWalk,
        deep: bool,
    ) -> Vec<WriterHit> {
        let snap = self.snapshot();
        let Some(content) = snap.graphs.get(document).cloned() else {
            return Vec::new();
        };
        let Some(content) = content.as_graph() else {
            return Vec::new();
        };
        let target_segments: Vec<&str> = target.split('.').collect();
        if target_segments.is_empty() {
            return Vec::new();
        }
        let analysis = self.graph_analysis(document);

        let mut hits: Vec<WriterHit> = Vec::new();
        for node in &content.nodes {
            let paths = NodePaths::new(node);
            if paths.output_path.is_some() && paths.output_prefix.is_empty() {
                continue;
            }
            let prefix_covers = paths.prefix_covers(&target_segments);
            let opaque_reads = matches!(paths.read_base, ReadBase::Opaque);

            match &node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    let mut row_ids: Vec<Arc<str>> = Vec::new();
                    if let Some(local) = paths.local_write_target(&target_segments) {
                        for row in content.expressions.iter() {
                            if row.key.is_empty() {
                                continue;
                            }
                            if PathMatch::key_overlaps(&row.key, local) {
                                row_ids.push(row.id.clone());
                            }
                        }
                    }
                    if !row_ids.is_empty() || prefix_covers {
                        let filter = (!prefix_covers).then_some(row_ids);
                        let reads = if deep {
                            self.node_global_reads(node, &paths, filter.as_deref())
                        } else {
                            Vec::new()
                        };
                        hits.push(WriterHit {
                            node_id: node.id.clone(),
                            reads,
                            nested: Vec::new(),
                            unresolved: opaque_reads,
                        });
                    }
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    let writes = prefix_covers
                        || paths
                            .local_write_target(&target_segments)
                            .is_some_and(|local| {
                                content.outputs.iter().any(|col| {
                                    !col.field.is_empty()
                                        && PathMatch::key_overlaps(&col.field, local)
                                })
                            });
                    if writes {
                        let reads = if deep {
                            self.node_global_reads(node, &paths, None)
                        } else {
                            Vec::new()
                        };
                        hits.push(WriterHit {
                            node_id: node.id.clone(),
                            reads,
                            nested: Vec::new(),
                            unresolved: opaque_reads,
                        });
                    }
                }
                DecisionNodeKind::DecisionNode { content: reference } => {
                    let local: Vec<&str> = if prefix_covers {
                        Vec::new()
                    } else if let Some(local) = paths.local_write_target(&target_segments) {
                        local.to_vec()
                    } else {
                        continue;
                    };
                    let Some(hit) =
                        self.call_writer_hit(node, &paths, reference, &local, walk, deep)
                    else {
                        continue;
                    };
                    hits.push(hit);
                }
                DecisionNodeKind::FunctionNode { content } => {
                    let Some(node_analysis) = analysis.as_ref().and_then(|a| a.nodes.get(&node.id))
                    else {
                        continue;
                    };
                    let output_type = node_analysis.output.resolve_at(target);
                    if matches!(output_type, VariableType::Any) {
                        continue;
                    }
                    let input_type = node_analysis.input.resolve_at(target);
                    if !matches!(input_type, VariableType::Any) {
                        continue;
                    }
                    let reads = if deep {
                        let source = crate::workspace::graph::function_source(content);
                        Self::function_input_reads(&source)
                            .into_iter()
                            .filter_map(|read| Self::map_read(&paths, &read))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    hits.push(WriterHit {
                        node_id: node.id.clone(),
                        reads,
                        nested: Vec::new(),
                        unresolved: opaque_reads,
                    });
                }
                _ => {}
            }
            if !deep && !hits.is_empty() {
                return hits;
            }
        }
        hits
    }

    fn call_writer_hit(
        &self,
        node: &DecisionNode,
        paths: &NodePaths,
        reference: &zen_types::decision::DecisionNodeContent,
        local: &[&str],
        walk: &mut DepWalk,
        deep: bool,
    ) -> Option<WriterHit> {
        let snap = self.snapshot();
        let callee: Arc<str> = Arc::from(reference.key.as_ref());
        let local_joined = local.join(".");
        let opaque_reads = matches!(paths.read_base, ReadBase::Opaque);

        if snap.graphs.contains_key(&callee) {
            if local.is_empty() {
                return Some(WriterHit {
                    node_id: node.id.clone(),
                    reads: Vec::new(),
                    nested: Vec::new(),
                    unresolved: opaque_reads,
                });
            }
            if deep {
                let child = self.graph_dep_node(&callee, &local_joined, walk);
                if child.written_by.is_none() {
                    return None;
                }
                return Some(WriterHit {
                    node_id: node.id.clone(),
                    reads: Vec::new(),
                    nested: child.deps,
                    unresolved: opaque_reads,
                });
            }
            if self
                .graph_written_by_guarded(&callee, &local_joined, walk)
                .is_none()
            {
                return None;
            }
            return Some(WriterHit {
                node_id: node.id.clone(),
                reads: Vec::new(),
                nested: Vec::new(),
                unresolved: opaque_reads,
            });
        }

        if snap.all_parsed.contains_key(reference.key.as_ref()) {
            let req = ScopeRequest::for_policy(callee.clone());
            let writes = local.is_empty()
                || self.outputs(&req).iter().any(|output| {
                    output.path.as_ref() == local_joined
                        || output.path.starts_with(&format!("{local_joined}."))
                        || local_joined.starts_with(&format!("{}.", output.path))
                });
            if !writes {
                return None;
            }
            let nested = if deep && !local.is_empty() {
                let child = self.dependencies(&local_joined);
                if child.written_by.is_some() {
                    child.deps
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            return Some(WriterHit {
                node_id: node.id.clone(),
                reads: Vec::new(),
                nested,
                unresolved: opaque_reads,
            });
        }

        None
    }

    pub(crate) fn node_global_reads(
        &self,
        node: &DecisionNode,
        paths: &NodePaths,
        expression_filter: Option<&[Arc<str>]>,
    ) -> Vec<Arc<str>> {
        let intellisense = self.graph_intellisense();
        let mut is = intellisense.borrow_mut();
        let mut out: Vec<Arc<str>> = Vec::new();
        for site in GraphAnalyzer::node_sites(node) {
            let is_transform_input = matches!(
                site.target,
                crate::workspace::types::CursorTarget::TransformInput
            );
            if let Some(filter) = expression_filter {
                if !is_transform_input
                    && !site
                        .expression_id
                        .as_ref()
                        .is_some_and(|id| filter.contains(id))
                {
                    continue;
                }
            }
            let (deps, references) = match site.kind {
                ExpressionKind::Standard => {
                    let result = is.dependencies(&site.source);
                    (result.reads, result.references)
                }
                ExpressionKind::Unary => (is.reads_unary(&site.source), Vec::new()),
            };
            for reference in references {
                if reference.path.first().map(|segment| segment.as_ref()) != Some("$") {
                    continue;
                }
                let rest = reference.path[1..]
                    .iter()
                    .map(|segment| segment.as_ref())
                    .collect::<Vec<_>>()
                    .join(".");
                if rest.is_empty() {
                    continue;
                }
                let prefix = paths.output_prefix.join(".");
                let global = if prefix.is_empty() {
                    rest
                } else {
                    format!("{prefix}.{rest}")
                };
                out.push(Arc::from(global));
            }
            let mut flat: Vec<PropertyRead> = Vec::new();
            ReadFlattener::extend_from_deps(&deps, &None, &mut flat);
            for read in flat {
                if read.via_alias || read.unresolved {
                    continue;
                }
                let path = read.path.as_ref();
                if path == "$" || path == "$root" {
                    continue;
                }
                if let Some(rest) = path.strip_prefix("$.") {
                    let mut global = paths.output_prefix.join(".");
                    if global.is_empty() {
                        global = rest.to_string();
                    } else {
                        global = format!("{global}.{rest}");
                    }
                    out.push(Arc::from(global));
                    continue;
                }
                if path.starts_with('$') {
                    continue;
                }
                if is_transform_input {
                    out.push(Arc::from(path));
                    continue;
                }
                if let Some(global) = Self::map_read(paths, path) {
                    out.push(global);
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    pub(crate) fn node_local_reads(
        &self,
        node: &DecisionNode,
        expression_filter: Option<&[Arc<str>]>,
    ) -> Vec<Arc<str>> {
        let intellisense = self.graph_intellisense();
        let mut is = intellisense.borrow_mut();
        let mut out: Vec<Arc<str>> = Vec::new();
        for site in GraphAnalyzer::node_sites(node) {
            let is_transform_input = matches!(
                site.target,
                crate::workspace::types::CursorTarget::TransformInput
            );
            if is_transform_input {
                continue;
            }
            if let Some(filter) = expression_filter {
                if !site
                    .expression_id
                    .as_ref()
                    .is_some_and(|id| filter.contains(id))
                {
                    continue;
                }
            }
            let (deps, references) = match site.kind {
                ExpressionKind::Standard => {
                    let result = is.dependencies(&site.source);
                    (result.reads, result.references)
                }
                ExpressionKind::Unary => (is.reads_unary(&site.source), Vec::new()),
            };
            for reference in references {
                if reference.path.first().map(|segment| segment.as_ref()) != Some("$") {
                    continue;
                }
                if reference.path.len() < 2 {
                    continue;
                }
                let joined = reference
                    .path
                    .iter()
                    .map(|segment| segment.as_ref())
                    .collect::<Vec<_>>()
                    .join(".");
                out.push(Arc::from(joined));
            }
            let mut flat: Vec<PropertyRead> = Vec::new();
            ReadFlattener::extend_from_deps(&deps, &None, &mut flat);
            for read in flat {
                if read.via_alias || read.unresolved {
                    continue;
                }
                let path = read.path.as_ref();
                if path.starts_with('$') {
                    continue;
                }
                out.push(Arc::from(path));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn map_read(paths: &NodePaths, path: &str) -> Option<Arc<str>> {
        match &paths.read_base {
            ReadBase::NodeInput => Some(Arc::from(path)),
            ReadBase::Opaque => None,
            ReadBase::Prefixed(prefix) => Some(Arc::from(format!("{}.{path}", prefix.join(".")))),
        }
    }

    pub(crate) fn function_input_reads(source: &str) -> Vec<String> {
        let bytes: Vec<char> = source.chars().collect();
        let mask = crate::workspace::graph::editor::js_code_mask(&bytes);
        let mut out: Vec<String> = Vec::new();
        let needle: Vec<char> = "input.".chars().collect();
        let mut i = 0usize;
        while i + needle.len() <= bytes.len() {
            if !mask[i] || bytes[i..i + needle.len()] != needle[..] {
                i += 1;
                continue;
            }
            let boundary_ok = i == 0 || {
                let prev = bytes[i - 1];
                !(prev.is_alphanumeric() || prev == '_' || prev == '$' || prev == '.')
            };
            if !boundary_ok {
                i += needle.len();
                continue;
            }
            let mut j = i + needle.len();
            let mut path = String::new();
            while j < bytes.len() {
                let c = bytes[j];
                if c.is_alphanumeric() || c == '_' || c == '$' {
                    path.push(c);
                    j += 1;
                } else if c == '.'
                    && j + 1 < bytes.len()
                    && (bytes[j + 1].is_alphabetic() || bytes[j + 1] == '_')
                {
                    path.push(c);
                    j += 1;
                } else {
                    break;
                }
            }
            if !path.is_empty() && !path.ends_with('.') {
                out.push(path);
            }
            i = j.max(i + 1);
        }
        out.sort();
        out.dedup();
        out
    }
}

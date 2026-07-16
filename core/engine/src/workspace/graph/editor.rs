use std::collections::VecDeque;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet, HashSetExt};
use zen_expression::variable::VariableType;
use zen_types::decision::{DecisionNode, DecisionNodeKind, TransformAttributes};

use crate::policy::blocks::IntelliSenseSource;
use crate::workspace::db::Db;
use crate::workspace::editor::{RenameRewrites, RenameSite};
use crate::workspace::graph::analysis::GraphAnalyzer;
use crate::workspace::types::{
    Cursor, CursorTarget, EngineEdit, PrepareRename, ReferenceKind, ReferenceSite, RenameTarget,
    Span, SpanOps,
};

pub(crate) enum ReadBase {
    NodeInput,
    Prefixed(Vec<String>),
    Opaque,
}

pub(crate) struct NodePaths {
    pub(crate) read_base: ReadBase,
    pub(crate) output_prefix: Vec<String>,
    pub(crate) output_path: Option<Arc<str>>,
}

impl NodePaths {
    pub(crate) fn new(node: &DecisionNode) -> Self {
        let attributes = Self::attributes(node);
        let read_base = match attributes.and_then(|a| a.input_field.as_ref()) {
            None => ReadBase::NodeInput,
            Some(field) => match Self::simple_path(field) {
                Some(segments) => ReadBase::Prefixed(segments),
                None => ReadBase::Opaque,
            },
        };
        let output_path = attributes.and_then(|a| a.output_path.clone());
        let output_prefix = output_path
            .as_ref()
            .and_then(|p| Self::simple_path(p))
            .unwrap_or_default();
        Self {
            read_base,
            output_prefix,
            output_path,
        }
    }

    pub(crate) fn attributes(node: &DecisionNode) -> Option<&TransformAttributes> {
        match &node.kind {
            DecisionNodeKind::ExpressionNode { content } => Some(&content.transform_attributes),
            DecisionNodeKind::DecisionTableNode { content } => Some(&content.transform_attributes),
            DecisionNodeKind::DecisionNode { content } => Some(&content.transform_attributes),
            _ => None,
        }
    }

    fn simple_path(source: &str) -> Option<Vec<String>> {
        let segments: Vec<String> = source.split('.').map(str::to_string).collect();
        let valid = segments.iter().all(|segment| {
            let mut chars = segment.chars();
            chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && chars.all(|c| c.is_alphanumeric() || c == '_')
        });
        valid.then_some(segments)
    }

    fn strip_segments<'a>(prefix: &[String], target: &'a [&'a str]) -> Option<&'a [&'a str]> {
        if target.len() <= prefix.len() {
            return None;
        }
        let matches = prefix.iter().zip(target).all(|(p, seg)| p == seg);
        matches.then(|| &target[prefix.len()..])
    }

    pub(crate) fn local_write_target<'a>(&self, target: &'a [&'a str]) -> Option<&'a [&'a str]> {
        Self::strip_segments(&self.output_prefix, target)
    }

    pub(crate) fn local_read_target<'a>(&self, target: &'a [&'a str]) -> Option<&'a [&'a str]> {
        match &self.read_base {
            ReadBase::NodeInput => Some(target),
            ReadBase::Opaque => None,
            ReadBase::Prefixed(prefix) => Self::strip_segments(prefix, target),
        }
    }

    pub(crate) fn prefix_covers(&self, target: &[&str]) -> bool {
        !self.output_prefix.is_empty()
            && target.len() <= self.output_prefix.len()
            && target.iter().zip(&self.output_prefix).all(|(t, p)| t == p)
    }
}

pub(crate) struct PathMatch;

impl PathMatch {
    pub(crate) fn segment_span(path: &str, index: usize) -> Span {
        let mut start = 0u32;
        for (i, segment) in path.split('.').enumerate() {
            let len = SpanOps::char_len(segment);
            if i == index {
                return (start, start + len);
            }
            start += len + 1;
        }
        (0, SpanOps::char_len(path))
    }

    pub(crate) fn key_matches(key: &str, local_target: &[&str]) -> Option<usize> {
        let segments: Vec<&str> = key.split('.').collect();
        if segments.len() < local_target.len() {
            return None;
        }
        let matches = local_target
            .iter()
            .zip(&segments)
            .all(|(target, seg)| target == seg);
        matches.then_some(local_target.len() - 1)
    }

    pub(crate) fn key_covers(key: &str, local_target: &[&str]) -> bool {
        let segments: Vec<&str> = key.split('.').collect();
        segments.len() < local_target.len()
            && segments.iter().zip(local_target).all(|(seg, t)| seg == t)
    }

    pub(crate) fn key_overlaps(key: &str, local_target: &[&str]) -> bool {
        Self::key_matches(key, local_target).is_some() || Self::key_covers(key, local_target)
    }
}

impl Db {
    pub(crate) fn graph_prepare_rename(&self, cursor: &Cursor) -> Option<PrepareRename> {
        let (source, kind, scope) = self.graph_resolve_cursor(cursor)?;
        let snap = self.snapshot();
        let doc = snap.graphs.get(&cursor.policy_path)?.clone();
        let content = doc.as_graph()?;
        let node = content.nodes.iter().find(|n| n.id == cursor.block_id)?;
        let paths = NodePaths::new(node);
        let dollar_local = matches!(node.kind, DecisionNodeKind::ExpressionNode { .. })
            && matches!(cursor.target, CursorTarget::Expression { .. });

        let intellisense = self.graph_intellisense();
        let analysis =
            IntelliSenseSource::analyze(&mut intellisense.borrow_mut(), &source, kind, &scope);

        for reference in &analysis.references {
            if reference.via_alias.is_some() || reference.via_index.is_some() {
                continue;
            }
            let segments: Vec<&str> = reference.path.iter().map(|s| s.as_ref()).collect();
            for (i, &span) in reference.spans.iter().enumerate() {
                let span = SpanOps::char_span(&source, span);
                if !(span.0 <= cursor.pos && cursor.pos < span.1) {
                    continue;
                }
                let global: Option<Vec<&str>> = if segments.first() == Some(&"$") {
                    (dollar_local && i >= 1).then(|| {
                        let mut global: Vec<&str> =
                            paths.output_prefix.iter().map(String::as_str).collect();
                        global.extend(&segments[1..=i]);
                        global
                    })
                } else if matches!(cursor.target, CursorTarget::TransformInput) {
                    Some(segments[..=i].to_vec())
                } else {
                    match &paths.read_base {
                        ReadBase::NodeInput => Some(segments[..=i].to_vec()),
                        ReadBase::Opaque => None,
                        ReadBase::Prefixed(prefix) => {
                            let mut global: Vec<&str> = prefix.iter().map(String::as_str).collect();
                            global.extend(&segments[..=i]);
                            Some(global)
                        }
                    }
                };
                let Some(global) = global else {
                    continue;
                };
                if global.first().is_some_and(|root| root.starts_with('$')) {
                    continue;
                }
                let path: Arc<str> = Arc::from(global.join("."));
                let mut visited: HashSet<Arc<str>> = HashSet::new();
                let target =
                    self.graph_resolve_property_target(&cursor.policy_path, &path, &mut visited)?;
                return Some(PrepareRename { target, span });
            }
        }
        None
    }

    pub(crate) fn graph_references(
        &self,
        document: &Arc<str>,
        path: &Arc<str>,
    ) -> Vec<ReferenceSite> {
        let mut sites: Vec<ReferenceSite> = self
            .graph_rename_sites(document, path)
            .into_iter()
            .map(RenameSite::into_reference)
            .map(Self::clip_reference_line)
            .collect();
        if let Some((node_id, key)) = self.graph_schema_declaration(document, path) {
            sites.push(ReferenceSite {
                policy_path: document.clone(),
                block_id: node_id,
                expression_id: None,
                source: key.clone(),
                span: (0, SpanOps::char_len(&key)),
                kind: ReferenceKind::DataModel,
            });
        }
        sites.sort_by(ReferenceSite::display_cmp);
        sites
    }

    pub(crate) fn graph_rename(
        &self,
        document: &Arc<str>,
        path: &Arc<str>,
        new_name: &str,
    ) -> Vec<EngineEdit> {
        let mut edits = self.replace_node_edits(self.graph_rename_sites(document, path), new_name);
        edits.extend(self.graph_schema_rename_edit(document, path, new_name));
        edits
    }

    fn graph_schema_declaration(
        &self,
        document: &Arc<str>,
        path: &str,
    ) -> Option<(Arc<str>, Arc<str>)> {
        let snap = self.snapshot();
        let doc = snap.graphs.get(document)?.clone();
        let content = doc.as_graph()?;
        for node in &content.nodes {
            let DecisionNodeKind::InputNode { content } = &node.kind else {
                continue;
            };
            let Some(schema) = &content.schema else {
                continue;
            };
            let parsed = Self::schema_value(schema)?;
            if Self::schema_walk(&parsed, path).is_some() {
                let key: Arc<str> = Arc::from(path.rsplit('.').next().unwrap_or(path));
                return Some((node.id.clone(), key));
            }
        }
        None
    }

    fn graph_schema_rename_edit(
        &self,
        document: &Arc<str>,
        path: &str,
        new_name: &str,
    ) -> Option<EngineEdit> {
        let snap = self.snapshot();
        let doc = snap.graphs.get(document)?.clone();
        let content = doc.as_graph()?;
        for node in &content.nodes {
            let DecisionNodeKind::InputNode { content } = &node.kind else {
                continue;
            };
            let Some(schema) = &content.schema else {
                continue;
            };
            let mut parsed = Self::schema_value(schema)?;
            if !Self::schema_rename(&mut parsed, path, new_name) {
                continue;
            }
            let Ok(mut node_json) = serde_json::to_value(node.as_ref()) else {
                continue;
            };
            node_json["content"]["schema"] = parsed;
            return Some(EngineEdit::ReplaceNode {
                document: document.clone(),
                node_id: node.id.clone(),
                new_node: node_json,
            });
        }
        None
    }

    fn schema_value(schema: &serde_json::Value) -> Option<serde_json::Value> {
        match schema {
            serde_json::Value::String(raw) => serde_json::from_str(raw).ok(),
            other => Some(other.clone()),
        }
    }

    fn schema_step<'a>(
        cursor: &'a serde_json::Value,
        segment: &str,
    ) -> Option<&'a serde_json::Value> {
        if let Some(found) = cursor.get("properties").and_then(|p| p.get(segment)) {
            return Some(found);
        }
        cursor
            .get("items")
            .and_then(|items| items.get("properties"))
            .and_then(|p| p.get(segment))
    }

    fn schema_step_mut<'a>(
        cursor: &'a mut serde_json::Value,
        segment: &str,
    ) -> Option<&'a mut serde_json::Value> {
        if cursor
            .get("properties")
            .and_then(|p| p.get(segment))
            .is_some()
        {
            return cursor
                .get_mut("properties")
                .and_then(|p| p.get_mut(segment));
        }
        cursor
            .get_mut("items")
            .and_then(|items| items.get_mut("properties"))
            .and_then(|p| p.get_mut(segment))
    }

    fn schema_owner_mut(
        cursor: &mut serde_json::Value,
    ) -> Option<&mut serde_json::Map<String, serde_json::Value>> {
        if cursor.get("properties").is_some() {
            return cursor.get_mut("properties").and_then(|p| p.as_object_mut());
        }
        cursor
            .get_mut("items")
            .and_then(|items| items.get_mut("properties"))
            .and_then(|p| p.as_object_mut())
    }

    fn schema_walk<'a>(schema: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
        let mut cursor = schema;
        for segment in path.split('.') {
            cursor = Self::schema_step(cursor, segment)?;
        }
        Some(cursor)
    }

    fn schema_rename(schema: &mut serde_json::Value, path: &str, new_name: &str) -> bool {
        let segments: Vec<&str> = path.split('.').collect();
        let Some((&last, parents)) = segments.split_last() else {
            return false;
        };
        let mut cursor = &mut *schema;
        for segment in parents {
            let Some(next) = Self::schema_step_mut(cursor, segment) else {
                return false;
            };
            cursor = next;
        }
        let required_holder = cursor.get("properties").is_none() && cursor.get("items").is_some();
        let Some(properties) = Self::schema_owner_mut(cursor) else {
            return false;
        };
        if !properties.contains_key(last) || properties.contains_key(new_name) {
            return false;
        }
        let Some(entry) = properties.remove(last) else {
            return false;
        };
        properties.insert(new_name.to_string(), entry);
        let required_scope = if required_holder {
            cursor.get_mut("items")
        } else {
            Some(cursor)
        };
        if let Some(required) = required_scope
            .and_then(|scope| scope.get_mut("required"))
            .and_then(|r| r.as_array_mut())
        {
            for item in required.iter_mut() {
                if item.as_str() == Some(last) {
                    *item = serde_json::Value::String(new_name.to_string());
                }
            }
        }
        true
    }

    pub(crate) fn graph_node_references(
        &self,
        document: &Arc<str>,
        node_id: &Arc<str>,
    ) -> Vec<ReferenceSite> {
        let mut sites: Vec<ReferenceSite> = self
            .graph_node_usage_sites(document, node_id)
            .into_iter()
            .map(RenameSite::into_reference)
            .collect();
        sites.sort_by(ReferenceSite::display_cmp);
        sites
    }

    pub(crate) fn graph_node_rename(
        &self,
        document: &Arc<str>,
        node_id: &Arc<str>,
        new_name: &str,
    ) -> Vec<EngineEdit> {
        let sites = self.graph_node_usage_sites(document, node_id);
        let mut edits = self.replace_node_edits(sites, new_name);
        if self.graph_node_name(document, node_id).is_none() {
            return edits;
        }
        let existing = edits.iter_mut().find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                document: doc,
                node_id: id,
                new_node,
            } if doc == document && id == node_id => Some(new_node),
            _ => None,
        });
        match existing {
            Some(new_node) => {
                new_node["name"] = serde_json::Value::String(new_name.to_string());
            }
            None => {
                let snap = self.snapshot();
                let Some(node) = snap
                    .graphs
                    .get(document)
                    .and_then(|d| d.as_graph())
                    .and_then(|c| c.nodes.iter().find(|n| n.id == *node_id))
                else {
                    return edits;
                };
                let Ok(mut node_json) = serde_json::to_value(node.as_ref()) else {
                    return edits;
                };
                node_json["name"] = serde_json::Value::String(new_name.to_string());
                edits.push(EngineEdit::ReplaceNode {
                    document: document.clone(),
                    node_id: node_id.clone(),
                    new_node: node_json,
                });
            }
        }
        edits
    }

    fn graph_node_name(&self, document: &Arc<str>, node_id: &Arc<str>) -> Option<Arc<str>> {
        let snap = self.snapshot();
        let doc = snap.graphs.get(document)?.clone();
        let content = doc.as_graph()?;
        content
            .nodes
            .iter()
            .find(|node| node.id == *node_id)
            .map(|node| node.name.clone())
    }

    fn graph_node_usage_sites(&self, document: &Arc<str>, node_id: &Arc<str>) -> Vec<RenameSite> {
        let snap = self.snapshot();
        let Some(doc) = snap.graphs.get(document).cloned() else {
            return Vec::new();
        };
        let Some(content) = doc.as_graph() else {
            return Vec::new();
        };
        let Some(name) = content
            .nodes
            .iter()
            .find(|node| node.id == *node_id)
            .map(|node| node.name.clone())
        else {
            return Vec::new();
        };
        if name.is_empty() {
            return Vec::new();
        }

        let intellisense = self.graph_intellisense();
        let mut seen: HashSet<(Arc<str>, Option<Arc<str>>, Span)> = HashSet::new();
        let mut sites: Vec<RenameSite> = Vec::new();
        for node in &content.nodes {
            for site in GraphAnalyzer::node_sites(node) {
                let mut push = |span: Span| {
                    if !seen.insert((node.id.clone(), site.expression_id.clone(), span)) {
                        return;
                    }
                    sites.push(RenameSite {
                        policy_path: document.clone(),
                        block_id: node.id.clone(),
                        expression_id: site.expression_id.clone(),
                        source: site.source.clone(),
                        span,
                        kind: ReferenceKind::ExpressionRead,
                    });
                };
                let analysis = IntelliSenseSource::analyze(
                    &mut intellisense.borrow_mut(),
                    &site.source,
                    site.kind,
                    &VariableType::empty_object(),
                );
                for reference in &analysis.references {
                    if reference.via_alias.is_some() || reference.via_index.is_some() {
                        continue;
                    }
                    let segments: Vec<&str> = reference.path.iter().map(|s| s.as_ref()).collect();
                    if segments.len() >= 2
                        && segments[0] == "$nodes"
                        && segments[1] == name.as_ref()
                    {
                        if let Some(&span) = reference.spans.get(1) {
                            let span = SpanOps::char_span(&site.source, span);
                            let quoted = site
                                .source
                                .chars()
                                .nth(span.0 as usize)
                                .is_some_and(|c| c == '\'' || c == '"');
                            if !quoted {
                                push(span);
                            }
                        }
                    }
                }
                for span in Self::bracket_name_spans(&site.source, &name) {
                    push(span);
                }
            }
        }
        sites
    }

    fn bracket_name_spans(source: &str, name: &str) -> Vec<Span> {
        let chars: Vec<char> = source.chars().collect();
        let name_chars: Vec<char> = name.chars().collect();
        let needle: Vec<char> = "$nodes[".chars().collect();
        let mut out: Vec<Span> = Vec::new();
        let mut i = 0usize;
        while i + needle.len() <= chars.len() {
            if chars[i..i + needle.len()] != needle[..] {
                i += 1;
                continue;
            }
            let mut j = i + needle.len();
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let Some(&quote) = chars.get(j) else {
                break;
            };
            if quote != '\'' && quote != '"' {
                i = j;
                continue;
            }
            let start = j + 1;
            let end = start + name_chars.len();
            if end <= chars.len()
                && chars[start..end] == name_chars[..]
                && chars.get(end) == Some(&quote)
            {
                out.push((start as u32, end as u32));
            }
            i = j + 1;
        }
        out
    }

    fn clip_reference_line(site: ReferenceSite) -> ReferenceSite {
        if !site.source.contains('\n') {
            return site;
        }
        let chars: Vec<char> = site.source.chars().collect();
        let span_start = site.span.0 as usize;
        let line_start = chars[..span_start.min(chars.len())]
            .iter()
            .rposition(|&c| c == '\n')
            .map_or(0, |at| at + 1);
        let line_end = chars[span_start.min(chars.len())..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(chars.len(), |at| span_start + at);
        let line: String = chars[line_start..line_end].iter().collect();
        let lead = line.chars().take_while(|c| c.is_whitespace()).count() as u32;
        let shift = (line_start as u32 + lead).min(site.span.0);
        ReferenceSite {
            source: Arc::from(line.trim_start()),
            span: (site.span.0 - shift, site.span.1 - shift),
            ..site
        }
    }

    fn graph_rename_sites(&self, document: &Arc<str>, path: &Arc<str>) -> Vec<RenameSite> {
        let mut sites = self.graph_collect_sites(document, path);
        self.extend_with_caller_reads(vec![(document.clone(), path.clone())], &mut sites);
        sites
    }

    fn extend_with_caller_reads(
        &self,
        seeds: Vec<(Arc<str>, Arc<str>)>,
        out: &mut Vec<RenameSite>,
    ) {
        let mut visited: HashSet<(Arc<str>, Arc<str>)> = seeds.iter().cloned().collect();
        let mut queue: VecDeque<(Arc<str>, Arc<str>)> = seeds.into();
        while let Some((callee, callee_path)) = queue.pop_front() {
            for (caller_doc, caller_path) in self.graph_caller_paths(&callee, &callee_path) {
                if !visited.insert((caller_doc.clone(), caller_path.clone())) {
                    continue;
                }
                let sites = self.graph_collect_sites(&caller_doc, &caller_path);
                if sites.iter().any(|s| s.kind == ReferenceKind::WriteKey) {
                    continue;
                }
                out.extend(sites);
                queue.push_back((caller_doc, caller_path));
            }
        }
    }

    fn graph_caller_paths(&self, callee: &str, callee_path: &str) -> Vec<(Arc<str>, Arc<str>)> {
        let snap = self.snapshot();
        let mut out: Vec<(Arc<str>, Arc<str>)> = Vec::new();
        let mut docs: Vec<(&Arc<str>, &Arc<crate::model::DecisionContent>)> =
            snap.graphs.iter().collect();
        docs.sort_by(|a, b| a.0.cmp(b.0));
        for (doc_path, doc) in docs {
            if doc_path.as_ref() == callee {
                continue;
            }
            let Some(content) = doc.as_graph() else {
                continue;
            };
            for node in &content.nodes {
                let DecisionNodeKind::DecisionNode { content: reference } = &node.kind else {
                    continue;
                };
                if reference.key.as_ref() != callee {
                    continue;
                }
                let paths = NodePaths::new(node);
                if paths.output_path.is_some() && paths.output_prefix.is_empty() {
                    continue;
                }
                let caller_path = if paths.output_prefix.is_empty() {
                    Arc::from(callee_path)
                } else {
                    Arc::from(format!("{}.{}", paths.output_prefix.join("."), callee_path))
                };
                out.push((doc_path.clone(), caller_path));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn graph_resolve_property_target(
        &self,
        document: &Arc<str>,
        path: &Arc<str>,
        visited: &mut HashSet<Arc<str>>,
    ) -> Option<RenameTarget> {
        if !visited.insert(document.clone()) {
            return None;
        }
        let local_sites = self.graph_collect_sites(document, path);
        if local_sites
            .iter()
            .any(|s| s.kind == ReferenceKind::WriteKey)
        {
            return Some(RenameTarget::GraphProperty {
                document: document.clone(),
                path: path.clone(),
            });
        }

        let snap = self.snapshot();
        let doc = snap.graphs.get(document)?.clone();
        let content = doc.as_graph()?;
        let target: Vec<&str> = path.split('.').collect();
        for node in &content.nodes {
            let DecisionNodeKind::DecisionNode { content: reference } = &node.kind else {
                continue;
            };
            let paths = NodePaths::new(node);
            if paths.output_path.is_some() && paths.output_prefix.is_empty() {
                continue;
            }
            let local: Vec<&str> = if paths.output_prefix.is_empty() {
                target.clone()
            } else {
                if target.len() <= paths.output_prefix.len()
                    || !paths.output_prefix.iter().zip(&target).all(|(p, t)| p == t)
                {
                    continue;
                }
                target[paths.output_prefix.len()..].to_vec()
            };
            let callee: Arc<str> = Arc::from(reference.key.as_ref());
            if snap.graphs.contains_key(&callee) {
                let local_path: Arc<str> = Arc::from(local.join("."));
                if let Some(resolved) =
                    self.graph_resolve_property_target(&callee, &local_path, visited)
                {
                    return Some(resolved);
                }
            } else if snap.all_parsed.contains_key(reference.key.as_ref()) {
                if let Some(resolved) = self.policy_property_target(&callee, &local) {
                    return Some(resolved);
                }
            }
        }
        None
    }

    fn policy_property_target(&self, policy: &Arc<str>, local: &[&str]) -> Option<RenameTarget> {
        use crate::workspace::types::ScopeRequest;
        let req = ScopeRequest::for_policy(policy.clone());
        let root = *local.first()?;
        let entities = self.entities(&req);
        let entity = entities.iter().find(|e| e.name.as_ref() == root);
        match (local.len(), entity) {
            (1, Some(entity)) => Some(RenameTarget::Entity {
                name: entity.name.clone(),
            }),
            (2, Some(entity)) => {
                let field = entity.fields.iter().find(|f| f.name.as_ref() == local[1])?;
                Some(RenameTarget::Field {
                    entity: entity.name.clone(),
                    field: field.name.clone(),
                })
            }
            (1, None) => {
                let is_global = self.globals(&req).iter().any(|g| g.name.as_ref() == root)
                    || self
                        .outputs(&req)
                        .iter()
                        .any(|o| o.path.split('.').next().is_some_and(|r| r == root));
                is_global.then(|| RenameTarget::Global {
                    name: Arc::from(root),
                })
            }
            _ => None,
        }
    }

    pub(crate) fn policy_caller_sites(&self, target: &RenameTarget) -> Vec<RenameSite> {
        let segments: Vec<Arc<str>> = match target {
            RenameTarget::Entity { name } => vec![name.clone()],
            RenameTarget::Field { entity, field } => vec![entity.clone(), field.clone()],
            RenameTarget::Global { name } => vec![name.clone()],
            RenameTarget::GraphProperty { .. } | RenameTarget::GraphNode { .. } => {
                return Vec::new()
            }
        };
        let joined = segments
            .iter()
            .map(|s| s.as_ref())
            .collect::<Vec<&str>>()
            .join(".");

        let snap = self.snapshot();
        let mut seeds: Vec<(Arc<str>, Arc<str>)> = Vec::new();
        let mut docs: Vec<(&Arc<str>, &Arc<crate::model::DecisionContent>)> =
            snap.graphs.iter().collect();
        docs.sort_by(|a, b| a.0.cmp(b.0));
        for (doc_path, doc) in docs {
            let Some(content) = doc.as_graph() else {
                continue;
            };
            for node in &content.nodes {
                let DecisionNodeKind::DecisionNode { content: reference } = &node.kind else {
                    continue;
                };
                if !snap.all_parsed.contains_key(reference.key.as_ref()) {
                    continue;
                }
                let callee: Arc<str> = Arc::from(reference.key.as_ref());
                if !self.policy_exposes(&callee, target) {
                    continue;
                }
                let paths = NodePaths::new(node);
                if paths.output_path.is_some() && paths.output_prefix.is_empty() {
                    continue;
                }
                let caller_path: Arc<str> = if paths.output_prefix.is_empty() {
                    Arc::from(joined.as_str())
                } else {
                    Arc::from(format!("{}.{}", paths.output_prefix.join("."), joined))
                };
                seeds.push((doc_path.clone(), caller_path));
            }
        }
        seeds.sort();
        seeds.dedup();

        let mut out: Vec<RenameSite> = Vec::new();
        let mut expanded: Vec<(Arc<str>, Arc<str>)> = Vec::new();
        for (doc_path, caller_path) in seeds {
            let sites = self.graph_collect_sites(&doc_path, &caller_path);
            if sites.iter().any(|s| s.kind == ReferenceKind::WriteKey) {
                continue;
            }
            out.extend(sites);
            expanded.push((doc_path, caller_path));
        }
        self.extend_with_caller_reads(expanded, &mut out);
        out
    }

    fn policy_exposes(&self, policy: &Arc<str>, target: &RenameTarget) -> bool {
        use crate::workspace::types::ScopeRequest;
        let req = ScopeRequest::for_policy(policy.clone());
        match target {
            RenameTarget::Entity { name } => self
                .entities(&req)
                .iter()
                .any(|e| e.name.as_ref() == name.as_ref()),
            RenameTarget::Field { entity, field } => self.entities(&req).iter().any(|e| {
                e.name.as_ref() == entity.as_ref()
                    && e.fields.iter().any(|f| f.name.as_ref() == field.as_ref())
            }),
            RenameTarget::Global { name } => {
                self.globals(&req)
                    .iter()
                    .any(|g| g.name.as_ref() == name.as_ref())
                    || self.outputs(&req).iter().any(|o| {
                        o.path
                            .split('.')
                            .next()
                            .is_some_and(|root| root == name.as_ref())
                    })
            }
            RenameTarget::GraphProperty { .. } | RenameTarget::GraphNode { .. } => false,
        }
    }

    pub(crate) fn replace_node_edits(
        &self,
        sites: Vec<RenameSite>,
        new_name: &str,
    ) -> Vec<EngineEdit> {
        let snap = self.snapshot();
        let mut per_node: HashMap<(Arc<str>, Arc<str>), Vec<RenameSite>> = HashMap::new();
        for site in sites {
            per_node
                .entry((site.policy_path.clone(), site.block_id.clone()))
                .or_default()
                .push(site);
        }

        let mut keys: Vec<(Arc<str>, Arc<str>)> = per_node.keys().cloned().collect();
        keys.sort();

        let mut edits: Vec<EngineEdit> = Vec::new();
        for key in keys {
            let Some(sites) = per_node.remove(&key) else {
                continue;
            };
            let (doc_path, node_id) = key;
            let Some(content) = snap.graphs.get(&doc_path).and_then(|d| d.as_graph()) else {
                continue;
            };
            let Some(node) = content.nodes.iter().find(|n| n.id == node_id) else {
                continue;
            };
            let Ok(mut node_json) = serde_json::to_value(node.as_ref()) else {
                continue;
            };
            RenameRewrites::from_sites(&sites, new_name)
                .protecting_node_keys()
                .apply_to(&mut node_json);
            edits.push(EngineEdit::ReplaceNode {
                document: doc_path,
                node_id,
                new_node: node_json,
            });
        }
        edits
    }

    fn graph_collect_sites(&self, document: &Arc<str>, path: &str) -> Vec<RenameSite> {
        let snap = self.snapshot();
        let Some(doc) = snap.graphs.get(document).cloned() else {
            return Vec::new();
        };
        let Some(content) = doc.as_graph() else {
            return Vec::new();
        };
        let target: Vec<&str> = path.split('.').collect();
        if target.is_empty() {
            return Vec::new();
        }

        let mut sites: Vec<RenameSite> = Vec::new();
        let mut emit = |node_id: &Arc<str>,
                        expression_id: Option<Arc<str>>,
                        source: &Arc<str>,
                        span: Span,
                        kind: ReferenceKind| {
            sites.push(RenameSite {
                policy_path: document.clone(),
                block_id: node_id.clone(),
                expression_id,
                source: source.clone(),
                span,
                kind,
            });
        };

        for node in &content.nodes {
            let paths = NodePaths::new(node);

            if let Some(output_path) = &paths.output_path {
                if paths.prefix_covers(&target) {
                    let span = PathMatch::segment_span(output_path, target.len() - 1);
                    emit(&node.id, None, output_path, span, ReferenceKind::WriteKey);
                }
            }

            if let Some(local_write) = paths.local_write_target(&target) {
                let write_keys: Vec<(Arc<str>, Arc<str>)> = match &node.kind {
                    DecisionNodeKind::ExpressionNode { content } => content
                        .expressions
                        .iter()
                        .map(|row| (row.id.clone(), row.key.clone()))
                        .collect(),
                    DecisionNodeKind::DecisionTableNode { content } => content
                        .outputs
                        .iter()
                        .map(|col| (col.id.clone(), col.field.clone()))
                        .collect(),
                    _ => Vec::new(),
                };
                for (id, key) in write_keys {
                    if key.is_empty() {
                        continue;
                    }
                    if let Some(index) = PathMatch::key_matches(&key, local_write) {
                        let span = PathMatch::segment_span(&key, index);
                        emit(&node.id, Some(id), &key, span, ReferenceKind::WriteKey);
                    }
                }
            }

            let local_read = paths.local_read_target(&target);
            let local_dollar = paths.local_write_target(&target);
            let is_expression_node = matches!(node.kind, DecisionNodeKind::ExpressionNode { .. });
            let intellisense = self.graph_intellisense();
            for site in GraphAnalyzer::node_sites(node) {
                let read_target: Option<&[&str]> =
                    if matches!(site.target, CursorTarget::TransformInput) {
                        Some(&target)
                    } else {
                        local_read
                    };
                let analysis = IntelliSenseSource::analyze(
                    &mut intellisense.borrow_mut(),
                    &site.source,
                    site.kind,
                    &VariableType::empty_object(),
                );
                for reference in &analysis.references {
                    if reference.via_alias.is_some() || reference.via_index.is_some() {
                        continue;
                    }
                    let segments: Vec<&str> = reference.path.iter().map(|s| s.as_ref()).collect();
                    if segments.first() == Some(&"$") {
                        let dollar_applies = is_expression_node
                            && !matches!(site.target, CursorTarget::TransformInput);
                        let Some(local) = local_dollar.filter(|_| dollar_applies) else {
                            continue;
                        };
                        if segments.len() < local.len() + 1 {
                            continue;
                        }
                        let matches = local.iter().zip(&segments[1..]).all(|(t, seg)| t == seg);
                        if !matches {
                            continue;
                        }
                        if let Some(&span) = reference.spans.get(local.len()) {
                            emit(
                                &node.id,
                                site.expression_id.clone(),
                                &site.source,
                                SpanOps::char_span(&site.source, span),
                                ReferenceKind::ExpressionRead,
                            );
                        }
                        continue;
                    }
                    let Some(read_target) = read_target else {
                        continue;
                    };
                    if segments.len() < read_target.len() {
                        continue;
                    }
                    let matches = read_target.iter().zip(&segments).all(|(t, seg)| t == seg);
                    if !matches {
                        continue;
                    }
                    if let Some(&span) = reference.spans.get(read_target.len() - 1) {
                        emit(
                            &node.id,
                            site.expression_id.clone(),
                            &site.source,
                            SpanOps::char_span(&site.source, span),
                            ReferenceKind::ExpressionRead,
                        );
                    }
                }
            }

            if let DecisionNodeKind::FunctionNode { content } = &node.kind {
                let source = crate::workspace::graph::function_source(content);
                if let Some(local) = paths.local_read_target(&target) {
                    let last_len = local.last().map_or(0, |segment| SpanOps::char_len(segment));
                    if last_len > 0 {
                        let needle = format!("input.{}", local.join("."));
                        for span in Self::function_read_spans(&source, &needle, last_len) {
                            emit(&node.id, None, &source, span, ReferenceKind::ExpressionRead);
                        }
                    }
                }
            }
        }
        sites
    }

    fn function_read_spans(source: &str, needle: &str, last_len: u32) -> Vec<Span> {
        let chars: Vec<char> = source.chars().collect();
        let mask = js_code_mask(&chars);
        let needle_chars: Vec<char> = needle.chars().collect();
        let mut out: Vec<Span> = Vec::new();
        let mut i = 0usize;
        while i + needle_chars.len() <= chars.len() {
            if !mask[i] || chars[i..i + needle_chars.len()] != needle_chars[..] {
                i += 1;
                continue;
            }
            let before_ok = i == 0 || {
                let prev = chars[i - 1];
                !(prev.is_alphanumeric() || prev == '_' || prev == '$' || prev == '.')
            };
            let end = i + needle_chars.len();
            let after_ok = end >= chars.len() || {
                let next = chars[end];
                !(next.is_alphanumeric() || next == '_' || next == '$')
            };
            if before_ok && after_ok {
                out.push(((end as u32) - last_len, end as u32));
            }
            i = end;
        }
        out
    }
}

pub(crate) fn js_code_mask(chars: &[char]) -> Vec<bool> {
    #[derive(PartialEq)]
    enum State {
        Code,
        Str(char),
        Template,
        TemplateExpr(u32),
        LineComment,
        BlockComment,
    }
    let mut mask = vec![true; chars.len()];
    let mut state = State::Code;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match state {
            State::Code => match c {
                '\'' | '"' => {
                    mask[i] = false;
                    state = State::Str(c);
                }
                '`' => {
                    mask[i] = false;
                    state = State::Template;
                }
                '/' if next == Some('/') => {
                    mask[i] = false;
                    state = State::LineComment;
                }
                '/' if next == Some('*') => {
                    mask[i] = false;
                    state = State::BlockComment;
                }
                _ => {}
            },
            State::Str(quote) => {
                mask[i] = false;
                if c == '\\' {
                    if let Some(slot) = mask.get_mut(i + 1) {
                        *slot = false;
                    }
                    i += 2;
                    continue;
                }
                if c == quote || c == '\n' {
                    state = State::Code;
                }
            }
            State::Template => {
                mask[i] = false;
                if c == '\\' {
                    if let Some(slot) = mask.get_mut(i + 1) {
                        *slot = false;
                    }
                    i += 2;
                    continue;
                }
                if c == '$' && next == Some('{') {
                    if let Some(slot) = mask.get_mut(i + 1) {
                        *slot = false;
                    }
                    i += 2;
                    state = State::TemplateExpr(0);
                    continue;
                }
                if c == '`' {
                    state = State::Code;
                }
            }
            State::TemplateExpr(depth) => match c {
                '{' => state = State::TemplateExpr(depth + 1),
                '}' => {
                    mask[i] = depth > 0;
                    state = if depth == 0 {
                        State::Template
                    } else {
                        State::TemplateExpr(depth - 1)
                    };
                }
                _ => {}
            },
            State::LineComment => {
                if c == '\n' {
                    state = State::Code;
                } else {
                    mask[i] = false;
                }
            }
            State::BlockComment => {
                mask[i] = false;
                if c == '*' && next == Some('/') {
                    if let Some(slot) = mask.get_mut(i + 1) {
                        *slot = false;
                    }
                    i += 2;
                    state = State::Code;
                    continue;
                }
            }
        }
        i += 1;
    }
    mask
}

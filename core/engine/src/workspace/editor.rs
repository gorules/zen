use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use serde_json::Value;
use zen_expression::intellisense::completion::Completions;
use zen_expression::intellisense::Reference;
use zen_expression::slot::SlotRole;
use zen_expression::variable::VariableType;

use crate::policy::blocks::{IntelliSenseSource, ROW_ID_KEY};
use crate::policy::ir::{DataModelIr, PropertyTypeIr};
use crate::policy::queries::scope::EntityGraph;
use crate::policy::raw::BlockDoc;
use crate::workspace::db::{Db, Snapshot};
use crate::workspace::types::{
    BlockRef, Completion, Cursor, CursorTarget, EngineEdit, ExpressionKind, InspectResult,
    PrepareRename, ReferenceKind, ReferenceSite, RenameTarget, Span, SpanOps,
};

impl Db {
    pub fn inspect(&self, cursor: &Cursor) -> Option<InspectResult> {
        let (source, kind, scope) = self.resolve_cursor(cursor)?;
        let role = self.cursor_scope(cursor)?.role;
        let r = self.cursor_intellisense(cursor).borrow_mut().inspect(
            &source,
            SpanOps::byte_offset(&source, cursor.pos) as u32,
            matches!(kind, ExpressionKind::Unary),
            role,
            &scope,
        )?;
        Some(InspectResult {
            span: SpanOps::char_span(&source, r.span),
            kind: r.kind,
            label: r.label,
            detail: r.detail,
            info: r.info,
        })
    }

    pub fn completions(&self, cursor: &Cursor) -> Vec<Completion> {
        let Some((source, _, _)) = self.resolve_cursor(cursor) else {
            return Vec::new();
        };
        let Some(scope) = self.cursor_scope(cursor) else {
            return Vec::new();
        };
        const MAX_PADDING: u32 = 1024;
        let len = SpanOps::char_len(&source);
        let cursor_pos = match cursor.pos > len.saturating_add(MAX_PADDING) {
            true => len,
            false => cursor.pos,
        };
        let padded;
        let source: &str = if cursor_pos > len {
            padded = format!("{source}{}", " ".repeat((cursor_pos - len) as usize));
            &padded
        } else {
            &source
        };
        let pos = SpanOps::byte_offset(source, cursor_pos) as u32;
        let result = self.cursor_intellisense(cursor).borrow_mut().slot(
            source,
            pos,
            matches!(scope.kind, ExpressionKind::Unary),
            scope.role,
            &scope.scope,
            scope.expected.as_ref(),
        );
        Completions::from_slot(source, pos, &scope.scope, &result.slot)
    }

    pub(crate) fn cursor_intellisense(
        &self,
        cursor: &Cursor,
    ) -> crate::policy::blocks::SharedIntelliSense {
        if self.is_graph(&cursor.policy_path) {
            self.graph_intellisense()
        } else {
            self.intellisense()
        }
    }

    pub fn prepare_rename(&self, cursor: &Cursor) -> Option<PrepareRename> {
        if self.is_graph(&cursor.policy_path) {
            return self.graph_prepare_rename(cursor);
        }
        if let Some(result) = self.prepare_rename_data_model(cursor) {
            return Some(result);
        }
        let unit = self.unit(&cursor.policy_path);
        let (source, kind, scope) = self.resolve_cursor(cursor)?;
        let intellisense = self.intellisense();
        let mut is = intellisense.borrow_mut();
        let analysis = IntelliSenseSource::analyze(&mut is, &source, kind, &scope);
        let mut found: Option<PrepareRename> = None;
        for reference in &analysis.references {
            unit.entity_graph.walk_segment_targets(reference, |i, t| {
                if found.is_some() {
                    return;
                }
                let Some(&span) = reference.spans.get(i) else {
                    return;
                };
                let span = SpanOps::char_span(&source, span);
                if span.0 <= cursor.pos && cursor.pos < span.1 {
                    found = Some(PrepareRename { target: t, span });
                }
            });
            if found.is_some() {
                break;
            }
        }
        found
    }

    pub fn rename(&self, target: &RenameTarget, new_name: &str) -> Vec<EngineEdit> {
        if let RenameTarget::GraphProperty { document, path } = target {
            return self.graph_rename(document, path, new_name);
        }
        if let RenameTarget::GraphNode { document, node_id } = target {
            return self.graph_node_rename(document, node_id, new_name);
        }
        let mut per_block: HashMap<BlockRef, Vec<RenameSite>> = HashMap::new();
        self.walk_renamable(target, |site| {
            let key = BlockRef {
                policy_path: site.policy_path.clone(),
                block_id: site.block_id.clone(),
            };
            per_block.entry(key).or_default().push(site);
        });
        let mut edits: Vec<EngineEdit> = per_block
            .into_iter()
            .filter_map(|(block_ref, sites)| self.build_replace_block(block_ref, sites, new_name))
            .collect();
        edits.extend(self.replace_node_edits(self.policy_caller_sites(target), new_name));
        edits
    }

    fn build_replace_block(
        &self,
        block_ref: BlockRef,
        sites: Vec<RenameSite>,
        new_name: &str,
    ) -> Option<EngineEdit> {
        let block = self.block_doc(&block_ref)?;
        let mut block_json = serde_json::to_value(&block).ok()?;

        let rewrites = RenameRewrites::from_sites(&sites, new_name);
        rewrites.apply_to(&mut block_json);

        Some(EngineEdit::ReplaceBlock {
            policy_path: block_ref.policy_path,
            block_id: block_ref.block_id,
            new_block: block_json,
        })
    }

    pub fn references(&self, target: &RenameTarget) -> Vec<ReferenceSite> {
        if let RenameTarget::GraphProperty { document, path } = target {
            return self.graph_references(document, path);
        }
        if let RenameTarget::GraphNode { document, node_id } = target {
            return self.graph_node_references(document, node_id);
        }
        let mut sites = Vec::new();
        self.walk_renamable(target, |site| {
            sites.push(site.into_reference());
        });
        sites.extend(
            self.policy_caller_sites(target)
                .into_iter()
                .map(RenameSite::into_reference),
        );
        sites.sort_by(ReferenceSite::display_cmp);
        sites
    }

    fn prepare_rename_data_model(&self, cursor: &Cursor) -> Option<PrepareRename> {
        let parsed = self.parsed(&cursor.policy_path)?;
        let dm: &DataModelIr = parsed
            .policy
            .data_models
            .iter()
            .find(|b| b.id == cursor.block_id)
            .map(|b| b.ir.as_ref())?;
        let is_global = dm.scope.is_global();
        let (target, name) = match &cursor.target {
            CursorTarget::DataModelName => {
                if is_global {
                    return None;
                }
                (
                    RenameTarget::Entity {
                        name: dm.name.clone(),
                    },
                    &dm.name,
                )
            }
            CursorTarget::DataModelProperty { id } => {
                let p = dm.properties.iter().find(|p| p.id == *id)?;
                let target = if is_global {
                    RenameTarget::Global {
                        name: p.name.clone(),
                    }
                } else {
                    RenameTarget::Field {
                        entity: dm.name.clone(),
                        field: p.name.clone(),
                    }
                };
                (target, &p.name)
            }
            _ => return None,
        };
        Some(PrepareRename {
            target,
            span: (0, SpanOps::char_len(name)),
        })
    }

    fn resolve_cursor(&self, cursor: &Cursor) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        if self.is_graph(&cursor.policy_path) {
            return self.graph_resolve_cursor(cursor);
        }
        let block_ref = BlockRef {
            policy_path: cursor.policy_path.clone(),
            block_id: cursor.block_id.clone(),
        };
        let mut scope = self.cursor_scope(cursor)?;
        if matches!(scope.role, SlotRole::Path) {
            scope.scope = self.enriched(&cursor.policy_path).scope.shallow_clone();
        }
        let policy = self.raw_policy(&cursor.policy_path)?;
        let block = policy
            .blocks
            .iter()
            .find(|b| b.id().is_some_and(|id| id == cursor.block_id.as_ref()))?;
        let source = match (block, &cursor.target) {
            (BlockDoc::Expression { data, .. }, CursorTarget::Expression { .. }) => {
                data.value.clone()
            }
            (BlockDoc::Expression { data, .. }, CursorTarget::ExpressionKey { .. }) => {
                data.key.clone()
            }
            (BlockDoc::Assertion { data, .. }, CursorTarget::Expression { id }) => data
                .conditions
                .iter()
                .find(|c| c.id == *id)?
                .expression
                .clone(),
            (BlockDoc::Assertion { data, .. }, CursorTarget::AssertionOutput) => {
                data.output.clone()
            }
            (BlockDoc::Match { data, .. }, CursorTarget::MatchTarget) => data.key.clone(),
            (BlockDoc::Match { data, .. }, CursorTarget::MatchValue { id }) => {
                data.arms.iter().find(|a| a.id == *id)?.value.clone()
            }
            (BlockDoc::Match { data, .. }, CursorTarget::Expression { id }) => {
                data.arms.iter().find(|a| a.id == *id)?.condition.clone()
            }
            (
                BlockDoc::DecisionTable { data, .. },
                CursorTarget::DecisionTableCell { row, col },
            ) => data
                .rules
                .iter()
                .find(|r| r.get(ROW_ID_KEY) == Some(row))?
                .get(col)
                .cloned()
                .unwrap_or_else(|| Arc::from("")),
            (BlockDoc::DecisionTable { data, .. }, CursorTarget::DecisionTableHead { col }) => {
                if let Some(input) = data.inputs.iter().find(|c| c.id == *col) {
                    input.field.clone().unwrap_or_else(|| Arc::from(""))
                } else {
                    data.outputs.iter().find(|c| c.id == *col)?.field.clone()
                }
            }
            _ => {
                return self
                    .block_ir(&block_ref)?
                    .resolve_cursor(cursor, scope.scope);
            }
        };
        Some((source, scope.kind, scope.scope))
    }
}

pub(crate) struct RenameSite {
    pub(crate) policy_path: Arc<str>,
    pub(crate) block_id: Arc<str>,
    pub(crate) expression_id: Option<Arc<str>>,
    pub(crate) source: Arc<str>,
    pub(crate) span: Span,
    pub(crate) kind: ReferenceKind,
}

impl RenameSite {
    pub(crate) fn into_reference(self) -> ReferenceSite {
        ReferenceSite {
            policy_path: self.policy_path,
            block_id: self.block_id,
            expression_id: self.expression_id,
            source: self.source,
            span: self.span,
            kind: self.kind,
        }
    }

    fn write_key_span(source: &str, target: &RenameTarget) -> Option<Span> {
        let source = source.strip_suffix("[]").unwrap_or(source);
        match target {
            RenameTarget::Global { name } => {
                (source == name.as_ref()).then_some((0, SpanOps::char_len(source)))
            }
            RenameTarget::Entity { name } => {
                let (src_entity, _) = source.split_once('.')?;
                (src_entity == name.as_ref()).then_some((0, SpanOps::char_len(name)))
            }
            RenameTarget::Field { entity, field } => {
                let (src_entity, rest) = source.split_once('.')?;
                let src_field = rest.split_once('.').map_or(rest, |(first, _)| first);
                if src_entity != entity.as_ref() || src_field != field.as_ref() {
                    return None;
                }
                let start = SpanOps::char_len(src_entity) + 1;
                Some((start, start + SpanOps::char_len(src_field)))
            }
            RenameTarget::GraphProperty { .. } | RenameTarget::GraphNode { .. } => None,
        }
    }
}

impl Db {
    fn walk_renamable(&self, target: &RenameTarget, mut callback: impl FnMut(RenameSite)) {
        let snap = self.snapshot();
        let intellisense = self.intellisense();

        let mut policies: Vec<Arc<str>> = snap.all_parsed.keys().cloned().collect();
        policies.sort();
        let units: Vec<(
            Arc<str>,
            std::sync::Arc<crate::workspace::db::Unit>,
            std::sync::Arc<crate::policy::queries::dependency::EnrichedState>,
        )> = policies
            .iter()
            .map(|p| {
                let unit = self.unit(p);
                let enriched = self.enriched_of_unit(&unit);
                (p.clone(), unit, enriched)
            })
            .collect();

        let mut is = intellisense.borrow_mut();
        for (policy_path, _, _) in &units {
            let parsed = &snap.all_parsed[policy_path];
            let contexts: Vec<_> = units
                .iter()
                .filter(|(owner, unit, _)| {
                    owner == policy_path || unit.members.contains(policy_path)
                })
                .collect();
            let mut seen: HashSet<(Arc<str>, Arc<str>, Span)> = HashSet::default();
            for rule in parsed.policy.rules() {
                for loc in rule.kind.expressions(&rule.id) {
                    for (_, unit, enriched) in &contexts {
                        let analysis = IntelliSenseSource::analyze(
                            &mut is,
                            &loc.source,
                            loc.kind,
                            &enriched.scope,
                        );
                        for reference in &analysis.references {
                            unit.entity_graph.walk_segment_targets(reference, |i, t| {
                                let Some(&span) = reference.spans.get(i).filter(|_| &t == target)
                                else {
                                    return;
                                };
                                let span = SpanOps::char_span(&loc.source, span);
                                let key = (loc.block_id.clone(), loc.expression_id.clone(), span);
                                if !seen.insert(key) {
                                    return;
                                }
                                callback(RenameSite {
                                    policy_path: policy_path.clone(),
                                    block_id: loc.block_id.clone(),
                                    expression_id: Some(loc.expression_id.clone()),
                                    source: loc.source.clone(),
                                    span,
                                    kind: ReferenceKind::ExpressionRead,
                                });
                            });
                        }
                    }
                }
                for (expression_id, source) in rule.kind.write_keys() {
                    if let Some(span) = RenameSite::write_key_span(&source, target) {
                        callback(RenameSite {
                            policy_path: policy_path.clone(),
                            block_id: rule.id.clone(),
                            expression_id,
                            source,
                            span,
                            kind: ReferenceKind::WriteKey,
                        });
                    }
                }
            }
            for (block_id, dm) in parsed.policy.data_models() {
                Snapshot::emit_dm_sites(policy_path, block_id, dm, target, &mut callback);
            }
        }
    }
}

impl Snapshot {
    fn emit_dm_sites(
        policy_path: &Arc<str>,
        block_id: &Arc<str>,
        dm: &DataModelIr,
        target: &RenameTarget,
        callback: &mut impl FnMut(RenameSite),
    ) {
        let mut emit = |expression_id, name: &Arc<str>| {
            callback(RenameSite {
                policy_path: policy_path.clone(),
                block_id: block_id.clone(),
                expression_id,
                source: name.clone(),
                span: (0, SpanOps::char_len(name)),
                kind: ReferenceKind::DataModel,
            });
        };
        match target {
            RenameTarget::Entity { name } => {
                if !dm.scope.is_global() && dm.name.as_ref() == name.as_ref() {
                    emit(None, &dm.name);
                }
                for prop in &dm.properties {
                    if let PropertyTypeIr::Relationship { target: t }
                    | PropertyTypeIr::Reference { target: t } = &prop.kind
                    {
                        if t.as_ref() == name.as_ref() {
                            emit(Some(prop.id.clone()), t);
                        }
                    }
                }
            }
            RenameTarget::Field { entity, field }
                if !dm.scope.is_global() && dm.name.as_ref() == entity.as_ref() =>
            {
                if let Some(p) = dm
                    .properties
                    .iter()
                    .find(|p| p.name.as_ref() == field.as_ref())
                {
                    emit(Some(p.id.clone()), &p.name);
                }
            }
            RenameTarget::Global { name } if dm.scope.is_global() => {
                if let Some(p) = dm
                    .properties
                    .iter()
                    .find(|p| p.name.as_ref() == name.as_ref())
                {
                    emit(Some(p.id.clone()), &p.name);
                }
            }
            _ => {}
        }
    }
}

impl EntityGraph {
    fn walk_segment_targets(
        &self,
        reference: &Reference,
        mut visit: impl FnMut(usize, RenameTarget),
    ) {
        let start: Option<(Arc<str>, usize)> = match (&reference.via_alias, &reference.via_index) {
            (Some(alias), _) => self
                .resolve_path_to_element(&alias.collection)
                .map(|e| (e, 1)),
            (_, Some(collection)) => self.resolve_path_to_element(collection).map(|e| (e, 0)),
            (None, None) => reference.path.first().and_then(|seg| {
                let first: Arc<str> = Arc::from(seg.as_ref());
                if self.contains(&first) {
                    visit(
                        0,
                        RenameTarget::Entity {
                            name: first.clone(),
                        },
                    );
                    return Some((first, 1));
                }
                if self.next_entity_for_global(&first).is_some()
                    || self.global_property(&first).is_some()
                {
                    visit(
                        0,
                        RenameTarget::Global {
                            name: first.clone(),
                        },
                    );
                    self.next_entity_for_global(&first)
                        .map(|target| (target, 1))
                } else {
                    None
                }
            }),
        };
        let Some((mut current, start_idx)) = start else {
            return;
        };
        for i in start_idx..reference.path.len() {
            let segment = reference.path[i].as_ref();
            visit(
                i,
                RenameTarget::Field {
                    entity: Arc::from(current.as_ref()),
                    field: Arc::from(segment),
                },
            );
            match self.next_entity(&current, segment) {
                Some(next) => current = next,
                None => break,
            }
        }
    }
}

pub(crate) struct RenameRewrites {
    by_source: HashMap<Arc<str>, String>,
    protect_node_keys: bool,
}

impl RenameRewrites {
    pub(crate) fn from_sites(sites: &[RenameSite], new_name: &str) -> Self {
        let mut spans_by_source: HashMap<Arc<str>, Vec<Span>> = HashMap::new();
        for site in sites {
            spans_by_source
                .entry(site.source.clone())
                .or_default()
                .push(site.span);
        }
        let by_source = spans_by_source
            .into_iter()
            .map(|(source, spans)| {
                let new = SpanOps::replace_at_char_spans(&source, &spans, new_name);
                (source, new)
            })
            .collect();
        Self {
            by_source,
            protect_node_keys: false,
        }
    }

    pub(crate) fn protecting_node_keys(mut self) -> Self {
        self.protect_node_keys = true;
        self
    }

    pub(crate) fn apply_to(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                if let Some(new) = self.by_source.get(s.as_str()) {
                    *s = new.clone();
                }
            }
            Value::Array(arr) => arr.iter_mut().for_each(|v| self.apply_to(v)),
            Value::Object(obj) => {
                for (key, child) in obj.iter_mut() {
                    if self.protect_node_keys
                        && matches!(
                            key.as_str(),
                            "id" | "_id"
                                | "type"
                                | "name"
                                | "kind"
                                | "sourceId"
                                | "targetId"
                                | "sourceHandle"
                                | "targetHandle"
                        )
                    {
                        continue;
                    }
                    if matches!(key.as_str(), "dataJson" | "schemaJson") {
                        self.apply_to_json_envelope(child);
                    } else {
                        self.apply_to(child);
                    }
                }
            }
            _ => {}
        }
    }

    fn apply_to_json_envelope(&self, value: &mut Value) {
        let Value::String(s) = value else {
            return;
        };
        let Ok(mut nested) = serde_json::from_str::<Value>(s) else {
            return;
        };
        let before = nested.clone();
        self.apply_to(&mut nested);
        if nested != before {
            *s = nested.to_string();
        }
    }
}

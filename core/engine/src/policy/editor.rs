use std::sync::Arc;

use ahash::{HashMap, HashMapExt};
use serde_json::Value;
use zen_expression::intellisense::Reference;
use zen_expression::nl::NlResult;
use zen_expression::variable::VariableType;

use crate::policy::blocks::IntelliSenseSource;
use crate::policy::db::{Db, Snapshot};
use crate::policy::ir::{DataModelIr, PropertyTypeIr};
use crate::policy::queries::scope::EntityGraph;
use crate::policy::types::{
    BlockRef, Completion, Cursor, CursorTarget, EngineEdit, ExpressionKind, InspectResult,
    NlExpression, PrepareRename, ReferenceKind, ReferenceSite, RenameTarget, Span, SpanOps,
};

impl Db {
    pub fn inspect(&self, cursor: &Cursor) -> Option<InspectResult> {
        let (source, _, scope) = self.resolve_cursor(cursor)?;
        let r = self
            .intellisense()
            .borrow_mut()
            .inspect(&source, cursor.pos, &scope)?;
        Some(InspectResult {
            span: r.span,
            kind: r.kind,
            label: r.label,
        })
    }

    pub fn completions(&self, cursor: &Cursor) -> Vec<Completion> {
        let Some((source, _, scope)) = self.resolve_cursor(cursor) else {
            return Vec::new();
        };
        self.intellisense()
            .borrow_mut()
            .completions(&source, cursor.pos, &scope)
    }

    pub fn nl(&self, policy: &str) -> Vec<NlExpression> {
        let policy_arc: Arc<str> = Arc::from(policy);
        let Some(parsed) = self.parsed(&policy_arc) else {
            return Vec::new();
        };
        let scope = self.enriched(policy).scope.shallow_clone();
        let labels = self.nl_label_resolver(policy);
        let intellisense = self.intellisense();
        let mut is = intellisense.borrow_mut();
        is.set_nl_labels(labels);
        let mut out = Vec::new();
        for rule in parsed.policy.rules() {
            out.extend(rule.nl(&policy_arc, &scope, &mut is));
        }
        is.set_nl_labels(None);
        out
    }

    pub fn nl_tokenize(&self, cursor: &Cursor, text: &str) -> Option<NlResult> {
        let (kind, scope) = self.nl_scope(cursor)?;
        let unary = matches!(kind, ExpressionKind::Unary);
        let labels = self.nl_label_resolver(&cursor.policy_path);
        let intellisense = self.intellisense();
        let mut is = intellisense.borrow_mut();
        is.set_nl_labels(labels);
        let mut result = is.nl_tokenize_scoped(&cursor.block_id, text, unary, &scope);
        if unary {
            let subject = scope.get("$");
            result.subject_options = is.nl_subject_options(&subject);
            result.subject_type = Some(subject);
        }
        is.set_nl_labels(None);
        Some(result)
    }

    fn nl_label_resolver(
        &self,
        policy: &str,
    ) -> Option<zen_expression::intellisense::NlLabelResolver> {
        let unit = self.unit(policy);
        if unit.dictionary_blocks.is_empty() {
            return None;
        }
        let mut labels: HashMap<Arc<str>, HashMap<Arc<str>, Arc<str>>> = HashMap::new();
        for (name, dict) in &unit.dictionaries {
            let entries: HashMap<Arc<str>, Arc<str>> = dict
                .entries
                .iter()
                .filter(|e| !e.label.is_empty())
                .map(|e| (e.value.clone(), e.label.clone()))
                .collect();
            if !entries.is_empty() {
                labels.insert(name.clone(), entries);
            }
        }
        if labels.is_empty() {
            return None;
        }
        Some(std::rc::Rc::new(move |name: &str, value: &str| {
            labels.get(name)?.get(value).map(|l| l.to_string())
        }))
    }

    fn nl_scope(&self, cursor: &Cursor) -> Option<(ExpressionKind, VariableType)> {
        let block = self.block_ir(&BlockRef {
            policy_path: cursor.policy_path.clone(),
            block_id: cursor.block_id.clone(),
        })?;
        let scope = self.enriched(&cursor.policy_path).scope.shallow_clone();
        let intellisense = self.intellisense();
        let mut is = intellisense.borrow_mut();
        Some(block.nl_scope(cursor, scope, &mut is))
    }

    pub fn prepare_rename(&self, cursor: &Cursor) -> Option<PrepareRename> {
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
        let mut per_block: HashMap<BlockRef, Vec<RenameSite>> = HashMap::new();
        self.walk_renamable(target, |site| {
            let key = BlockRef {
                policy_path: site.policy_path.clone(),
                block_id: site.block_id.clone(),
            };
            per_block.entry(key).or_default().push(site);
        });
        per_block
            .into_iter()
            .filter_map(|(block_ref, sites)| self.build_replace_block(block_ref, sites, new_name))
            .collect()
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
        let mut sites = Vec::new();
        self.walk_renamable(target, |site| {
            sites.push(site.into_reference());
        });
        sites.sort_by(|a, b| {
            a.kind
                .display_order()
                .cmp(&b.kind.display_order())
                .then_with(|| a.policy_path.cmp(&b.policy_path))
                .then_with(|| a.block_id.cmp(&b.block_id))
                .then_with(|| a.span.0.cmp(&b.span.0))
        });
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
        let rule = self.block_ir(&BlockRef {
            policy_path: cursor.policy_path.clone(),
            block_id: cursor.block_id.clone(),
        })?;
        let (source, kind, narrowed) = rule.resolve_cursor(
            cursor,
            self.enriched(&cursor.policy_path).scope.shallow_clone(),
        )?;
        (cursor.pos as usize <= source.chars().count()).then_some((source, kind, narrowed))
    }
}

struct RenameSite {
    policy_path: Arc<str>,
    block_id: Arc<str>,
    expression_id: Option<Arc<str>>,
    source: Arc<str>,
    span: Span,
    kind: ReferenceKind,
}

impl RenameSite {
    fn into_reference(self) -> ReferenceSite {
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
            std::sync::Arc<crate::policy::db::Unit>,
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
        for (policy_path, unit, enriched) in &units {
            let parsed = &snap.all_parsed[policy_path];
            let entities = &unit.entity_graph;
            let scope = &enriched.scope;
            for rule in parsed.policy.rules() {
                for loc in rule.kind.expressions(&rule.id) {
                    let analysis =
                        IntelliSenseSource::analyze(&mut is, &loc.source, loc.kind, scope);
                    for reference in &analysis.references {
                        entities.walk_segment_targets(reference, |i, t| {
                            let Some(&span) = reference.spans.get(i).filter(|_| &t == target)
                            else {
                                return;
                            };
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
                    match self.next_entity_for_global(&first) {
                        Some(target) => Some((target, 1)),
                        None => None,
                    }
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

struct RenameRewrites {
    by_source: HashMap<Arc<str>, String>,
}

impl RenameRewrites {
    fn from_sites(sites: &[RenameSite], new_name: &str) -> Self {
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
        Self { by_source }
    }

    fn apply_to(&self, value: &mut Value) {
        match value {
            Value::String(s) => {
                if let Some(new) = self.by_source.get(s.as_str()) {
                    *s = new.clone();
                }
            }
            Value::Array(arr) => arr.iter_mut().for_each(|v| self.apply_to(v)),
            Value::Object(obj) => {
                for (key, child) in obj.iter_mut() {
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

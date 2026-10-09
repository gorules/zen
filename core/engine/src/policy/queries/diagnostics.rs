use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};

use crate::analysis::nullable::NullableOperand;
use crate::policy::blocks::{AnalysisContext, BlockKind};
use crate::policy::raw::BlockDoc;
use crate::policy::ir::PropertyTypeIr;
use crate::policy::linter::Linter;
use crate::policy::queries::dependency::{RuleEnrichedAnalysis, WriteScope};
use crate::policy::queries::path::PathRoot;
use crate::workspace::db::{AnalysisPass, Db, Unit};
use crate::workspace::types::{
    BlockRef, Cursor, CursorTarget, Diagnostic, DiagnosticCode, DiagnosticLocation,
};

impl Db {
    pub fn compute_policy_diagnostics(&self, path: &Arc<str>) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = Vec::new();

        if let Some(parsed) = self.parsed(path) {
            out.extend(parsed.diagnostics.iter().cloned());
        }

        let unit = self.unit(path);
        if let Some(parsed) = self.parsed(path) {
            for rule in parsed.policy.rules() {
                rule.check_single_entity_scope(path, &unit.classifier, &mut out);
            }
        }
        out.extend(self.imported_context_diagnostics(path));

        let mut imported: Option<Vec<Diagnostic>> = None;
        for mut diagnostic in self.scope_diagnostics(path) {
            if !diagnostic.is_in(path) {
                let imported =
                    imported.get_or_insert_with(|| self.imported_scope_diagnostics(path));
                if imported.iter().any(|d| d.same_as(&diagnostic)) {
                    continue;
                }
                diagnostic.message = format!(
                    "in imported policy '{}': {}",
                    diagnostic.location.policy_path, diagnostic.message
                );
                diagnostic.location = DiagnosticLocation::policy(path.clone());
            }
            out.push(diagnostic);
        }

        let enriched = self.enriched(path);
        out.extend(
            enriched
                .diagnostics
                .iter()
                .filter(|d| d.is_in(path))
                .cloned(),
        );
        for rule in enriched
            .per_rule
            .iter()
            .filter(|rule| rule.policy_path == *path)
        {
            out.extend(self.rule_diagnostics(rule));
        }

        out.extend(self.import_diagnostics(path));

        out.extend(self.unreachable_reads_diagnostics(path));

        out.extend(self.nested_iteration_diagnostics(path));

        out.extend(Linter::standard().run(self, path));

        self.locate_nullable_sources(path, &mut out);

        out.extend(self.data_model_expression_diagnostics(path));

        out
    }

    /// Expressions on data model properties (features, per-event computes,
    /// model inputs and `when`), checked against what each can read.
    fn data_model_expression_diagnostics(&self, path: &Arc<str>) -> Vec<Diagnostic> {
        let Some(policy) = self.raw_policy(path) else {
            return Vec::new();
        };
        let intellisense = self.intellisense();
        let unit = self.unit(path);
        let dictionary_types = Rc::new(unit.dictionary_types());
        let declared_paths = Rc::new(unit.data_model_paths.clone());
        let mut out = Vec::new();
        for block in &policy.blocks {
            let BlockDoc::DataModel { id, data } = block else {
                continue;
            };
            // Own fields each derived feature or compute reads: name → (read, where).
            let own_names: HashSet<&str> = data.properties.iter().map(|p| p.name.as_ref()).collect();
            let mut depends: Vec<(&Arc<str>, Arc<str>, CursorTarget, Arc<str>, Option<(u32, u32)>)> =
                Vec::new();
            for prop in &data.properties {
                let mut targets = Vec::new();
                if prop.feature.is_some() {
                    targets.push(CursorTarget::FeatureExpr { id: prop.id.clone() });
                }
                if prop.compute.is_some() {
                    targets.push(CursorTarget::ComputeExpr { id: prop.id.clone() });
                }
                if let Some(model) = &prop.model {
                    if let Some(inputs) = model.get("inputs").and_then(serde_json::Value::as_object) {
                        targets.extend(inputs.keys().map(|input| CursorTarget::ModelInput {
                            id: prop.id.clone(),
                            input: Arc::from(input.as_str()),
                        }));
                    }
                    if model.get("when").is_some() {
                        targets.push(CursorTarget::ModelWhen { id: prop.id.clone() });
                    }
                    if model.get("request").is_some() && model.get("inputs").is_none() {
                        targets.push(CursorTarget::ModelRequest { id: prop.id.clone() });
                    }
                    if model.get("response").is_some() {
                        targets.push(CursorTarget::ModelResponse { id: prop.id.clone() });
                    }
                }
                for target in targets {
                    let Some(source) = data.expression(&target).filter(|s| !s.trim().is_empty())
                    else {
                        continue;
                    };
                    let cursor = Cursor {
                        policy_path: path.clone(),
                        block_id: id.clone(),
                        pos: 0,
                        target,
                    };
                    let Some(scope) = self.data_model_cursor_scope(&cursor) else {
                        continue;
                    };
                    // As a rule block's expression: type errors and unknown names.
                    let mut cx = AnalysisContext::new(
                        scope.scope,
                        path.clone(),
                        id.clone(),
                        intellisense.clone(),
                        AnalysisPass::Enriched,
                        dictionary_types.clone(),
                        Rc::default(),
                        declared_paths.clone(),
                    );
                    cx.with_target(Some(cursor.target.clone()), |cx| {
                        cx.analyze_standard(&source, Some(prop.id.clone()));
                    });
                    // Derived features and computes: null in, null out.
                    let propagates = matches!(cursor.target, CursorTarget::ComputeExpr { .. })
                        || matches!(cursor.target, CursorTarget::FeatureExpr { .. })
                            && prop.feature.as_ref().is_some_and(|f| {
                                f.window.as_ref().is_none_or(|w| w.list().is_empty())
                            });
                    let summary = cx.finish();
                    // Calls take part in the order too: what their request,
                    // `when` and response read (a call with named inputs reads
                    // the request, not the entity).
                    let orders = propagates
                        || match &cursor.target {
                            CursorTarget::ModelRequest { .. } | CursorTarget::ModelResponse { .. } => true,
                            CursorTarget::ModelWhen { .. } => prop
                                .model
                                .as_ref()
                                .is_some_and(|m| m.get("inputs").is_none()),
                            _ => false,
                        };
                    if orders {
                        for read in &summary.reads {
                            let first = read.path.split('.').next().unwrap_or_default();
                            if !read.via_alias && own_names.contains(first) {
                                depends.push((
                                    &prop.name,
                                    Arc::from(first),
                                    cursor.target.clone(),
                                    prop.id.clone(),
                                    read.span,
                                ));
                            }
                        }
                    }
                    let diagnostics = summary.diagnostics.into_iter().filter(|d| {
                        !(propagates && NullableOperand::null_propagates(d))
                    });
                    out.extend(diagnostics.map(|mut d| {
                        d.location.target.get_or_insert_with(|| cursor.target.clone());
                        d
                    }));
                }
            }
            out.extend(Self::derivation_cycles(path, id, &depends));
            out.extend(self.stored_relationship_diagnostics(path, id, data, &unit));
            // How long a call's reply may be reused: a duration like 5m.
            for prop in &data.properties {
                let Some(staleness) = prop.model.as_ref().and_then(|m| m.get("maxStaleness")) else {
                    continue;
                };
                let text = staleness.as_str().unwrap_or_default();
                if text.is_empty() || Self::is_staleness(text) {
                    continue;
                }
                out.push(Diagnostic::error(
                    DiagnosticCode::ParseError,
                    DiagnosticLocation::expression(path.clone(), id.clone(), prop.id.clone(), None),
                    format!(
                        "`maxStaleness` of call '{}' is a duration like 30s, 5m or 1h, not `{}`",
                        prop.name,
                        if staleness.is_string() { text.to_string() } else { staleness.to_string() }
                    ),
                ));
            }
        }
        out
    }

    /// A stored relationship (`on`: members by key; `through`: counterparties
    /// in events), checked against the entities it names.
    fn stored_relationship_diagnostics(
        &self,
        path: &Arc<str>,
        block_id: &Arc<str>,
        data: &crate::policy::raw::DataModelDoc,
        unit: &Unit,
    ) -> Vec<Diagnostic> {
        use crate::policy::ir::{PropertyTypeIr, Records};
        use crate::policy::raw::PropertyTypeDoc;
        let mut out = Vec::new();
        let key_parts: Vec<String> = match &data.key {
            Some(serde_json::Value::String(key)) => vec![key.clone()],
            Some(serde_json::Value::Array(parts)) => {
                parts.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()
            }
            _ => vec!["id".to_string()],
        };
        for prop in &data.properties {
            let PropertyTypeDoc::Relationship { target } = &prop.property_type else {
                continue;
            };
            let (on, through) = (prop.rest.get("on"), prop.rest.get("through"));
            if on.is_none() && through.is_none() {
                continue;
            }
            let mut problem = |message: String| {
                out.push(Diagnostic::error(
                    DiagnosticCode::ParseError,
                    DiagnosticLocation::expression(path.clone(), block_id.clone(), prop.id.clone(), None),
                    format!("`{}`: {message}", prop.name),
                ));
            };
            if on.is_some() && through.is_some() {
                problem("a stored relationship is found by key (`on`) or through events (`through`), not both".into());
                continue;
            }
            let Some(target_ir) = unit.entities.get(target) else {
                problem(format!("`{target}` isn't an entity"));
                continue;
            };
            if let Some(on) = on {
                let Some(pairs) = on.as_object() else {
                    problem("`on` maps the target's properties to this entity's key parts".into());
                    continue;
                };
                let mut used: Vec<&str> = Vec::new();
                for (member, part) in pairs {
                    let member_prop = target_ir.properties.iter().find(|p| p.name.as_ref() == member);
                    if member_prop.is_none_or(|p| p.supply.is_some() || p.is_stored()) {
                        problem(format!("`{member}` isn't a data property of `{target}`"));
                    }
                    match part.as_str() {
                        Some(part) if key_parts.iter().any(|k| k == part) => {
                            if used.contains(&part) {
                                problem(format!("key part `{part}` is matched twice"));
                            }
                            used.push(part);
                        }
                        Some(part) => problem(format!("`{part}` isn't a key part of `{}` ({})", data.name, key_parts.join(", "))),
                        None => problem(format!("`{member}` maps to a key part's name")),
                    }
                }
                for part in &key_parts {
                    if !used.contains(&part.as_str()) {
                        problem(format!("key part `{part}` has no member property matched to it"));
                    }
                }
            }
            if let Some(through) = through {
                let text = |key: &str| through.get(key).and_then(serde_json::Value::as_str);
                let events = text("events").unwrap_or_default();
                let events_ir = unit.entities.get(events);
                if events_ir.is_none_or(|e| e.records != Records::Events) {
                    problem(format!("`{events}` isn't an events entity"));
                }
                let reference_to = |field: &str| {
                    events_ir.and_then(|e| e.properties.iter().find(|p| p.name.as_ref() == field)).and_then(|p| {
                        match &p.kind {
                            PropertyTypeIr::Reference { target } => Some(target.clone()),
                            _ => None,
                        }
                    })
                };
                if let Some(events_ir) = events_ir.filter(|e| e.records == Records::Events) {
                    for (role, expected) in [("self", data.name.as_ref()), ("member", target.as_ref())] {
                        let field = text(role).unwrap_or_default();
                        if reference_to(field).as_deref() != Some(expected) {
                            problem(format!(
                                "`{role}` is `{field}`: it names a reference of `{}` to `{expected}`",
                                events_ir.name
                            ));
                        }
                    }
                }
                if !text("window").is_some_and(Self::is_duration) {
                    problem(format!(
                        "`window` is a duration like 10m, 1h or 30d, not `{}`",
                        text("window").unwrap_or_default()
                    ));
                }
            }
        }
        out
    }

    /// A positive whole number of minutes, hours or days: `10m`, `1h`, `30d`.
    fn is_duration(text: &str) -> bool {
        let Some((digits, unit)) = text.split_at_checked(text.len().saturating_sub(1)) else {
            return false;
        };
        matches!(unit, "m" | "h" | "d")
            && !digits.is_empty()
            && !digits.starts_with('0')
            && digits.bytes().all(|b| b.is_ascii_digit())
    }

    /// A positive whole number of seconds, minutes, hours or days: `30s`, `5m`, `5h`, `1d`.
    fn is_staleness(text: &str) -> bool {
        let Some((digits, unit)) = text.split_at_checked(text.len().saturating_sub(1)) else {
            return false;
        };
        matches!(unit, "s" | "m" | "h" | "d")
            && !digits.is_empty()
            && !digits.starts_with('0')
            && digits.bytes().all(|b| b.is_ascii_digit())
    }

    /// A derived field that needs its own value, directly (`x = x + 5`) or
    /// through others (`a = b`, `b = a`): no order computes it. Reported on
    /// each read that closes the loop.
    fn derivation_cycles(
        path: &Arc<str>,
        block_id: &Arc<str>,
        depends: &[(&Arc<str>, Arc<str>, CursorTarget, Arc<str>, Option<(u32, u32)>)],
    ) -> Vec<Diagnostic> {
        let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
        for (from, to, ..) in depends {
            edges.entry(from.as_ref()).or_default().push(to.as_ref());
        }
        // The way from `start` back to `goal`, if any.
        let way_back = |start: &str, goal: &str| -> Option<Vec<String>> {
            let mut seen: HashSet<&str> = HashSet::default();
            let mut stack: Vec<(&str, Vec<String>)> = vec![(start, vec![start.to_string()])];
            while let Some((at, trail)) = stack.pop() {
                if at == goal {
                    return Some(trail);
                }
                if !seen.insert(at) {
                    continue;
                }
                for next in edges.get(at).into_iter().flatten() {
                    let mut trail = trail.clone();
                    trail.push(next.to_string());
                    stack.push((next, trail));
                }
            }
            None
        };
        let mut out = Vec::new();
        for (from, to, target, prop_id, span) in depends {
            let message = if from.as_ref() == to.as_ref() {
                format!("`{from}` reads itself: it can't be computed from its own value")
            } else if let Some(trail) = way_back(to, from) {
                format!(
                    "`{from}` depends on itself: {from} → {}",
                    trail.join(" → ")
                )
            } else {
                continue;
            };
            out.push(
                Diagnostic::error(
                    DiagnosticCode::CyclicDependency,
                    DiagnosticLocation::expression(
                        path.clone(),
                        block_id.clone(),
                        prop_id.clone(),
                        *span,
                    )
                    .with_target(target.clone()),
                    message,
                ),
            );
        }
        out
    }

    fn rule_diagnostics(&self, rule: &RuleEnrichedAnalysis) -> Vec<Diagnostic> {
        if rule.table_checks.is_empty() {
            return rule.diagnostics.clone();
        }
        let block = self.block_ir(&BlockRef {
            policy_path: rule.policy_path.clone(),
            block_id: rule.block_id.clone(),
        });
        let Some(BlockKind::DecisionTable(table)) = block.as_ref().map(|block| &block.kind) else {
            return rule.diagnostics.clone();
        };
        let intellisense = self.intellisense();
        let mut out = Vec::with_capacity(rule.diagnostics.len());
        let mut cursor = 0;
        for check in &rule.table_checks {
            out.extend(rule.diagnostics[cursor..check.at].iter().cloned());
            out.extend(table.verify(
                check,
                &mut intellisense.borrow_mut(),
                &rule.policy_path,
                &rule.block_id,
            ));
            cursor = check.at;
        }
        out.extend(rule.diagnostics[cursor..].iter().cloned());
        out
    }

    fn locate_nullable_sources(&self, path: &Arc<str>, out: &mut [Diagnostic]) {
        if !out.iter().any(|d| d.args.contains_key("nullablePath")) {
            return;
        }
        let shallow = self.shallow();
        let unit = self.unit(path);
        let mut members: Vec<&Arc<str>> = unit.members.iter().collect();
        members.sort_by_key(|member| (*member != path, member.to_string()));
        let covers = |written: &str, field: &str| {
            field == written
                || field
                    .strip_prefix(written)
                    .is_some_and(|rest| rest.starts_with('.'))
        };
        for diagnostic in out.iter_mut() {
            if !diagnostic.is_in(path) {
                continue;
            }
            let Some(field) = diagnostic.args.get("nullablePath").cloned() else {
                continue;
            };
            let writer = shallow
                .per_rule
                .iter()
                .filter(|rule| unit.members.contains(&rule.policy_path))
                .find(|rule| rule.writes.iter().any(|w| covers(&w.path, &field)))
                .map(|rule| (rule.policy_path.clone(), rule.block_id.clone()));
            let declared = || {
                members.iter().find_map(|member| {
                    let parsed = self.parsed(member)?;
                    let found = parsed.policy.data_models().find_map(|(id, dm)| {
                        dm.properties
                            .iter()
                            .any(|prop| {
                                let declared = if dm.scope.is_global() {
                                    prop.name.to_string()
                                } else {
                                    format!("{}.{}", dm.name, prop.name)
                                };
                                declared == field
                            })
                            .then(|| ((*member).clone(), id.clone()))
                    });
                    found
                })
            };
            let Some((policy, block)) = writer.or_else(declared) else {
                continue;
            };
            diagnostic.args.insert("sourceId", block.to_string());
            if policy != *path {
                diagnostic.args.insert("sourcePolicy", policy.to_string());
            }
        }
    }

    pub fn evaluation_diagnostics(&self, entry: &Arc<str>) -> Vec<Diagnostic> {
        let unit = self.unit(entry);
        let enriched = self.enriched(entry);

        let mut members: Vec<&Arc<str>> = unit.members.iter().collect();
        members.sort();

        let mut candidates: Vec<Diagnostic> = Vec::new();
        for member in members {
            if let Some(parsed) = self.parsed(member) {
                candidates.extend(parsed.diagnostics.iter().cloned());
            }
            candidates.extend(self.unit_scoped_diagnostics(&unit, member));
            candidates.extend(self.import_diagnostics(member));
        }
        candidates.extend(self.scope_diagnostics(entry));
        candidates.extend(enriched.diagnostics.iter().cloned());
        candidates.extend(
            enriched
                .per_rule
                .iter()
                .flat_map(|rule| rule.diagnostics.iter().cloned()),
        );
        if !unit.dep_graph.cyclic_paths().is_empty() {
            candidates.push(Diagnostic::error(
                DiagnosticCode::CyclicDependency,
                DiagnosticLocation::policy(entry.clone()),
                "cyclic dependency detected among computed properties",
            ));
        }

        let mut out: Vec<Diagnostic> = Vec::new();
        for diagnostic in candidates {
            if !out.iter().any(|d| d.same_as(&diagnostic)) {
                out.push(diagnostic);
            }
        }
        out
    }

    fn imported_context_diagnostics(&self, path: &Arc<str>) -> Vec<Diagnostic> {
        let unit = self.unit(path);
        let mut members: Vec<&Arc<str>> = unit.members.iter().filter(|m| *m != path).collect();
        members.sort();

        let mut out = Vec::new();
        for member in members {
            let member_unit = self.unit(member);
            let own = self.unit_scoped_diagnostics(&member_unit, member);
            for mut diagnostic in self.unit_scoped_diagnostics(&unit, member) {
                if own.iter().any(|d| d.same_as(&diagnostic)) {
                    continue;
                }
                diagnostic.message = format!(
                    "in imported policy '{}': {}",
                    diagnostic.location.policy_path, diagnostic.message
                );
                diagnostic.location = DiagnosticLocation::policy(path.clone());
                out.push(diagnostic);
            }
        }
        out
    }

    fn unit_scoped_diagnostics(&self, unit: &Unit, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Some(parsed) = self.parsed(target) {
            for rule in parsed.policy.rules() {
                rule.check_single_entity_scope(target, &unit.classifier, &mut out);
            }
        }
        out.extend(self.nested_iteration_in(unit, target));
        out.extend(self.unreachable_reads_in(unit, target));
        out.extend(self.self_referencing_writes_in(unit, target));
        out
    }

    fn self_referencing_writes_in(&self, unit: &Unit, target: &Arc<str>) -> Vec<Diagnostic> {
        use crate::policy::queries::dependency::PathPrefix;

        let mut out = Vec::new();
        let data_model_paths = &unit.data_model_paths;
        for rule in self.shallow().rules_for(target) {
            let block = self.block_ir(&BlockRef {
                policy_path: rule.policy_path.clone(),
                block_id: rule.block_id.clone(),
            });
            for write in &rule.writes {
                let conflict = rule.reads.iter().find(|r| {
                    data_model_paths.matches_prefix(&r.path).is_none()
                        && PathPrefix::extends(&r.path, &write.path)
                });
                let Some(read) = conflict else {
                    continue;
                };
                let wtarget = block
                    .as_ref()
                    .and_then(|b| b.kind.write_target(&write.path));
                let message = if read.path == write.path {
                    format!("block reads and writes the same property '{}'", write.path)
                } else {
                    format!(
                        "block writes '{}' while reading the overlapping path '{}' — it would read a partially-built object",
                        write.path, read.path
                    )
                };
                out.push(Diagnostic::error(
                    DiagnosticCode::SelfReferencingWrite,
                    DiagnosticLocation::block(rule.policy_path.clone(), rule.block_id.clone())
                        .maybe_target(wtarget),
                    message,
                ));
            }
        }
        out
    }

    fn imported_scope_diagnostics(&self, path: &Arc<str>) -> Vec<Diagnostic> {
        let Some(parsed) = self.parsed(path) else {
            return Vec::new();
        };
        parsed
            .policy
            .imports()
            .iter()
            .filter(|import| import.as_ref() != path.as_ref())
            .flat_map(|import| self.scope_diagnostics(import))
            .collect()
    }

    fn imports_transitively(&self, from: &Arc<str>, to: &Arc<str>) -> bool {
        if from == to {
            return false;
        }
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        let mut stack: Vec<Arc<str>> = vec![from.clone()];
        while let Some(path) = stack.pop() {
            let Some(parsed) = self.parsed(&path) else {
                continue;
            };
            for import in parsed.policy.imports() {
                if import == to {
                    return true;
                }
                if seen.insert(import.clone()) {
                    stack.push(import.clone());
                }
            }
        }
        false
    }

    fn scope_diagnostics(&self, path: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = self.graph_diagnostics(path);
        out.extend(self.data_model_diagnostics(path));
        out.extend(self.dictionary_diagnostics(path));
        out
    }

    fn nested_iteration_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        self.nested_iteration_in(&self.unit(target), target)
    }

    fn nested_iteration_in(&self, unit: &Unit, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let shallow = self.shallow();
        let entity_sources = &unit.entity_sources;
        let classifier = &unit.classifier;

        for rule_analysis in shallow.rules_for(target) {
            let mut flagged: HashSet<Arc<str>> = HashSet::default();
            for write in &rule_analysis.writes {
                let PathRoot::Entity { entity, .. } = classifier.classify(&write.path) else {
                    continue;
                };
                let Some(src) = entity_sources.get(&entity) else {
                    continue;
                };
                let root = src.path.split('.').next().unwrap_or_default();
                if root == entity.as_ref() || !entity_sources.contains_key(root) {
                    continue;
                }
                if !flagged.insert(entity.clone()) {
                    continue;
                }
                out.push(Diagnostic::error(
                    DiagnosticCode::UnsupportedNestedIteration,
                    DiagnosticLocation::block(
                        rule_analysis.policy_path.clone(),
                        rule_analysis.block_id.clone(),
                    ),
                    format!(
                        "cannot write to entity '{entity}': its collection '{}' is nested inside iterated entity '{root}'; only one level of relationship nesting is evaluated",
                        src.path
                    ),
                ));
            }
        }
        out
    }

    fn unreachable_reads_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        self.unreachable_reads_in(&self.unit(target), target)
    }

    fn unreachable_reads_in(&self, unit: &Unit, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let shallow = self.shallow();
        let entity_sources = &unit.entity_sources;
        let classifier = &unit.classifier;
        let rule_index = self.rule_by_ref();

        for rule_analysis in shallow.rules_for(target) {
            let block_ref = BlockRef {
                policy_path: rule_analysis.policy_path.clone(),
                block_id: rule_analysis.block_id.clone(),
            };
            let Some(rule) = rule_index.get(&block_ref) else {
                continue;
            };
            let write_scope = rule.write_scope(&classifier);

            for read in &rule_analysis.reads {
                if read.via_alias {
                    continue;
                }
                let PathRoot::Entity {
                    entity: read_entity,
                    ..
                } = classifier.classify(&read.path)
                else {
                    continue;
                };
                if !entity_sources.contains_key(&read_entity) {
                    continue;
                }
                let reachable = matches!(&write_scope, WriteScope::Entity(e) if e.as_ref() == read_entity.as_ref());
                if reachable {
                    continue;
                }
                let context = match &write_scope {
                    WriteScope::Entity(e) => format!("entity '{e}'"),
                    WriteScope::Global => "globals".to_string(),
                    WriteScope::Empty | WriteScope::Mixed => "this block".to_string(),
                };
                out.push(Diagnostic::error(
                    DiagnosticCode::UnreachableEntityRead,
                    DiagnosticLocation::expression(
                        rule_analysis.policy_path.clone(),
                        rule_analysis.block_id.clone(),
                        read.expression_id.clone().unwrap_or_else(|| Arc::from("")),
                        read.span,
                    ),
                    format!(
                        "cannot read '{}' from {context}: entity '{read_entity}' is iterated; aggregate it with map/some/every/sum",
                        read.path
                    ),
                ));
            }
        }
        out
    }

    fn graph_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        use crate::policy::queries::dependency::PathPrefix;

        let mut out = Vec::new();
        let shallow = self.shallow();
        let unit = self.unit(target);
        let data_model_paths = &unit.data_model_paths;
        let visible = &unit.members;
        let mut first_writer: HashMap<Arc<str>, BlockRef> = HashMap::new();
        let mut all_writes: Vec<(BlockRef, bool, Arc<str>)> = Vec::new();

        let mut sorted_members: Vec<&Arc<str>> = visible.iter().collect();
        sorted_members.sort();
        for rule in sorted_members.iter().flat_map(|m| shallow.rules_for(m)) {
            let in_target = rule.is_in(target);
            let block_ref = BlockRef {
                policy_path: rule.policy_path.clone(),
                block_id: rule.block_id.clone(),
            };
            let block = self.block_ir(&block_ref);

            for write in &rule.writes {
                let wtarget = block
                    .as_ref()
                    .and_then(|b| b.kind.write_target(&write.path));

                if let Some(matched) = data_model_paths.matches_prefix(&write.path) {
                    out.push(Diagnostic::error(
                        DiagnosticCode::InputOverride,
                        DiagnosticLocation::block(rule.policy_path.clone(), rule.block_id.clone())
                            .maybe_target(wtarget.clone()),
                        format!(
                            "cannot write to '{}': '{}' is defined as a DataModel input",
                            write.path, matched
                        ),
                    ));
                    continue;
                }

                match first_writer.get(&write.path) {
                    Some(existing) => {
                        let (blamed, blamed_target) = if self
                            .imports_transitively(&existing.policy_path, &rule.policy_path)
                        {
                            let target = self
                                .block_ir(existing)
                                .and_then(|b| b.kind.write_target(&write.path));
                            (existing.clone(), target)
                        } else {
                            (block_ref.clone(), wtarget.clone())
                        };
                        out.push(Diagnostic::error(
                            DiagnosticCode::DuplicateWriter,
                            DiagnosticLocation::block(blamed.policy_path, blamed.block_id)
                                .maybe_target(blamed_target),
                            format!(
                                "property '{}' is written by both block '{}' (in '{}') and block '{}' (in '{}')",
                                write.path,
                                existing.block_id,
                                existing.policy_path,
                                rule.block_id,
                                rule.policy_path
                            ),
                        ));
                    }
                    None => {
                        first_writer.insert(write.path.clone(), block_ref.clone());
                    }
                }

                all_writes.push((block_ref.clone(), in_target, write.path.clone()));
            }
        }
        out.extend(self.self_referencing_writes_in(&unit, target));

        let mut containers: Vec<Arc<str>> = Vec::new();
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        for (_, _, candidate) in &all_writes {
            if !seen.insert(candidate.clone()) {
                continue;
            }
            let has_nested = all_writes
                .iter()
                .any(|(_, _, w)| w != candidate && PathPrefix::extends(candidate, w));
            if has_nested {
                containers.push(candidate.clone());
            }
        }
        containers.sort();
        for container in containers {
            let mut blocks: Vec<(&BlockRef, bool)> = Vec::new();
            for (block_ref, in_t, write) in &all_writes {
                if PathPrefix::extends(&container, write)
                    && !blocks.iter().any(|(b, _)| *b == block_ref)
                {
                    blocks.push((block_ref, *in_t));
                }
            }
            let Some((owner, _)) = blocks
                .iter()
                .find(|(_, in_t)| *in_t)
                .or_else(|| blocks.first())
            else {
                continue;
            };
            let cross_policy = blocks
                .iter()
                .any(|(b, _)| b.policy_path != owner.policy_path);
            let names: Vec<String> = blocks
                .iter()
                .map(|(b, _)| {
                    if cross_policy {
                        format!("{}:{}", b.policy_path, b.block_id)
                    } else {
                        b.block_id.to_string()
                    }
                })
                .collect();
            out.push(Diagnostic::error(
                DiagnosticCode::PartialObjectWrite,
                DiagnosticLocation::block(owner.policy_path.clone(), owner.block_id.clone()),
                format!(
                    "object '{}' is written as a whole and also written into via nested paths ({}); the whole-object write overwrites the nested writes — assemble it in one place or merge explicitly",
                    container,
                    names.join(", ")
                ),
            ));
        }

        let graph = &unit.dep_graph;
        let cyclic = graph.cyclic_paths();
        let owners: HashSet<Arc<str>> = cyclic
            .iter()
            .filter_map(|path| graph.writer_for(path))
            .map(|owner| owner.policy_path.clone())
            .collect();
        let owned_here = owners.contains(target);
        let seen_by_import = owners.iter().any(|owner| {
            let own = self.unit(owner).dep_graph.cyclic_paths();
            cyclic.iter().any(|path| own.contains(path))
        });
        if !cyclic.is_empty() && (owned_here || !seen_by_import) {
            out.push(Diagnostic::error(
                DiagnosticCode::CyclicDependency,
                DiagnosticLocation::policy(target.clone()),
                "cyclic dependency detected among computed properties",
            ));
        }

        out
    }

    fn import_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let Some(parsed) = self.parsed(target) else {
            return out;
        };
        let all_paths = self.path_set();

        for imported in parsed.policy.imports() {
            if !all_paths.contains(imported) {
                out.push(Diagnostic::error(
                    DiagnosticCode::ImportNotFound,
                    DiagnosticLocation::policy(target.clone()),
                    format!("imported policy '{}' not found in workspace", imported),
                ));
            }
        }

        if let Some(cycles) = self.import_cycles().get(target) {
            out.extend(cycles.iter().cloned());
        }
        out
    }

    fn data_model_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let mut seen: HashMap<
            (Option<Arc<str>>, Arc<str>),
            (Arc<str>, Arc<str>, PropertyTypeIr, bool, bool),
        > = HashMap::default();
        let unit = self.unit(target);
        let all_dms = &unit.data_models;
        let known_entities: HashSet<Arc<str>> = all_dms
            .iter()
            .filter(|e| !e.ir.scope.is_global())
            .map(|e| e.ir.name.clone())
            .collect();
        let global_property_names: HashSet<Arc<str>> = all_dms
            .iter()
            .filter(|e| e.ir.scope.is_global())
            .flat_map(|e| e.ir.properties.iter().map(|p| p.name.clone()))
            .collect();

        for entry in all_dms {
            let policy_path = &entry.policy_path;
            let block_id = &entry.block_id;
            let dm = &entry.ir;
            let is_global = dm.scope.is_global();

            if !is_global && global_property_names.contains(&dm.name) {
                out.push(Diagnostic::error(
                    DiagnosticCode::DataModelCollision,
                    DiagnosticLocation::block(policy_path.clone(), block_id.clone()),
                    format!(
                        "entity name '{}' collides with a global property of the same name",
                        dm.name
                    ),
                ));
            }

            for prop in &dm.properties {
                if is_global && known_entities.contains(&prop.name) {
                    out.push(Diagnostic::error(
                        DiagnosticCode::DataModelCollision,
                        DiagnosticLocation::expression(
                            policy_path.clone(),
                            block_id.clone(),
                            prop.id.clone(),
                            None,
                        ),
                        format!(
                            "global property '{}' collides with an entity of the same name",
                            prop.name
                        ),
                    ));
                }

                let key = if is_global {
                    (None, prop.name.clone())
                } else {
                    (Some(dm.name.clone()), prop.name.clone())
                };
                if let Some((prev_policy, prev_block, prev_kind, prev_array, prev_optional)) =
                    seen.get(&key).cloned()
                {
                    let conflicts = !prop.kind.same_shape_as(&prev_kind)
                        || prev_array != prop.array
                        || prev_optional != prop.optional;
                    if conflicts {
                        let location = if is_global {
                            format!("global property '{}'", prop.name)
                        } else {
                            format!("property '{}' in entity '{}'", prop.name, dm.name)
                        };
                        out.push(Diagnostic::error(
                            DiagnosticCode::DataModelCollision,
                            DiagnosticLocation::expression(
                                policy_path.clone(),
                                block_id.clone(),
                                prop.id.clone(),
                                None,
                            ),
                            format!(
                                "{location} conflicts with definition in '{prev_policy}' (block '{prev_block}')"
                            ),
                        ));
                    }
                } else {
                    seen.insert(
                        key,
                        (
                            policy_path.clone(),
                            block_id.clone(),
                            prop.kind.clone(),
                            prop.array,
                            prop.optional,
                        ),
                    );
                }

                if let PropertyTypeIr::Relationship { target: t }
                | PropertyTypeIr::Reference { target: t } = &prop.kind
                {
                    let dictionary_target =
                        matches!(prop.kind, PropertyTypeIr::Relationship { .. })
                            && unit.dictionaries.contains_key(t);
                    if !known_entities.contains(t) && !dictionary_target {
                        let owner = if is_global {
                            format!("global property '{}'", prop.name)
                        } else {
                            format!("property '{}' in entity '{}'", prop.name, dm.name)
                        };
                        out.push(Diagnostic::error(
                            DiagnosticCode::UnknownDataModelTarget,
                            DiagnosticLocation::expression(
                                policy_path.clone(),
                                block_id.clone(),
                                prop.id.clone(),
                                None,
                            ),
                            format!("{owner} references unknown entity '{t}'"),
                        ));
                    }
                }
            }
        }

        out
    }

    fn dictionary_diagnostics(&self, target: &Arc<str>) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let unit = self.unit(target);
        let known_entities: HashSet<Arc<str>> = unit
            .data_models
            .iter()
            .filter(|e| !e.ir.scope.is_global())
            .map(|e| e.ir.name.clone())
            .collect();
        let global_property_names: HashSet<Arc<str>> = unit
            .data_models
            .iter()
            .filter(|e| e.ir.scope.is_global())
            .flat_map(|e| e.ir.properties.iter().map(|p| p.name.clone()))
            .collect();

        let mut first_by_name: HashMap<Arc<str>, (Arc<str>, Arc<str>)> = HashMap::default();
        for entry in &unit.dictionary_blocks {
            let name = &entry.ir.name;
            if let Some((prev_policy, prev_block)) = first_by_name.get(name) {
                out.push(Diagnostic::error(
                        DiagnosticCode::DataModelCollision,
                        DiagnosticLocation::block(
                            entry.policy_path.clone(),
                            entry.block_id.clone(),
                        ),
                        format!(
                            "dictionary '{name}' is already defined in '{prev_policy}' (block '{prev_block}')"
                        ),
                    ));
                continue;
            }
            first_by_name.insert(
                name.clone(),
                (entry.policy_path.clone(), entry.block_id.clone()),
            );

            if known_entities.contains(name) {
                out.push(Diagnostic::error(
                    DiagnosticCode::DataModelCollision,
                    DiagnosticLocation::block(entry.policy_path.clone(), entry.block_id.clone()),
                    format!("dictionary name '{name}' collides with an entity of the same name"),
                ));
            }
            if global_property_names.contains(name) {
                out.push(Diagnostic::error(
                    DiagnosticCode::DataModelCollision,
                    DiagnosticLocation::block(entry.policy_path.clone(), entry.block_id.clone()),
                    format!(
                        "dictionary name '{name}' collides with a global property of the same name"
                    ),
                ));
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::workspace::types::{DiagnosticCode, Severity};
    use crate::workspace::Workspace;

    #[test]
    fn evaluation_diagnostics_skip_table_verification() {
        let policy = |value: &str| {
            json!({ "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "applicant",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                    ]
                } } },
                { "id": "dt", "type": "decisionTable", "props": { "data": {
                    "hitPolicy": "first",
                    "inputs": [ { "id": "i0", "name": "Age", "field": "applicant.age" } ],
                    "outputs": [ { "id": "o0", "name": "Rate", "field": "applicant.rate" } ],
                    "rules": [
                        { "_id": "r1", "i0": "< 18", "o0": "1" },
                        { "_id": "r2", "i0": "< 10", "o0": "2" }
                    ]
                } } },
                { "id": "calc", "type": "expression", "props": { "data": { "key": "applicant.total", "value": value } } }
            ] })
        };
        let table_codes = [
            DiagnosticCode::MissingCases,
            DiagnosticCode::UnreachableRule,
        ];
        for (value, errors) in [("applicant.age + 1", 0), ("applicant.missing > 50", 1)] {
            let mut ws = Workspace::new();
            ws.set_policy("p", serde_json::from_value(policy(value)).expect("policy"));
            let editor = ws.diagnostics("p");
            assert_eq!(
                editor
                    .iter()
                    .filter(|d| table_codes.contains(&d.code))
                    .count(),
                2,
                "{editor:?}"
            );
            let evaluation = ws.evaluation_diagnostics("p");
            assert!(
                evaluation.iter().all(|d| !table_codes.contains(&d.code)),
                "{evaluation:?}"
            );
            assert_eq!(
                evaluation
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .count(),
                errors,
                "{evaluation:?}"
            );
        }
    }
}

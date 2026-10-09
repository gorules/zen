use std::rc::Rc;
use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};
use zen_expression::variable::VariableType;

use crate::analysis::nullable::NullableOperand;

use crate::policy::blocks::{
    AnalysisContext, BlockKind, IntelliSenseSource, ReadFlattener, SharedIntelliSense,
};
use crate::policy::ir::{is_duration, PropertyTypeIr};
use crate::policy::linter::Linter;
use crate::policy::queries::dependency::{RuleEnrichedAnalysis, WriteScope};
use crate::policy::queries::path::PathRoot;
use crate::policy::raw::{call_inputs, BlockDoc, PropertyDoc};
use crate::workspace::db::{AnalysisPass, Db, Unit};
use crate::workspace::types::{
    BlockRef, Cursor, CursorTarget, Diagnostic, DiagnosticCode, DiagnosticLocation, ExpressionKind,
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
        // What each derived field and call reads, across the unit's entities
        // (`transaction.risk_score` → `card.card_score`).
        let mut depends: Vec<Depend> = Vec::new();
        for block in &policy.blocks {
            let BlockDoc::DataModel { id, data } = block else {
                continue;
            };
            let own_names: HashSet<&str> =
                data.properties.iter().map(|p| p.name.as_ref()).collect();
            for prop in &data.properties {
                for target in Self::host_targets(prop) {
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
                    let scope_type = scope.scope.shallow_clone();
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
                    let propagates = Self::propagates_null(prop, &cursor.target);
                    let summary = cx.finish();
                    if Self::orders(prop, &cursor.target) {
                        for read in summary.reads.iter().filter(|read| !read.via_alias) {
                            let mut reads = Self::derivation_reads(&unit, &data.name, &read.path);
                            let first = read.path.split('.').next().unwrap_or_default();
                            if reads.is_empty() && own_names.contains(first) {
                                reads.push(Arc::from(format!("{}.{first}", data.name)));
                            }
                            depends.extend(reads.into_iter().map(|to| Depend {
                                from: Arc::from(format!("{}.{}", data.name, prop.name)),
                                to,
                                at: Some(DependSite {
                                    block_id: id.clone(),
                                    target: cursor.target.clone(),
                                    prop_id: prop.id.clone(),
                                    span: read.span,
                                }),
                            }));
                        }
                    }
                    if matches!(cursor.target, CursorTarget::FeatureExpr { .. }) {
                        out.extend(Self::feature_shape(
                            path,
                            id,
                            prop,
                            &source,
                            &scope_type,
                            &intellisense,
                        ));
                    }
                    let diagnostics = summary
                        .diagnostics
                        .into_iter()
                        .filter(|d| !(propagates && NullableOperand::null_propagates(d)));
                    out.extend(diagnostics.map(|mut d| {
                        d.location
                            .target
                            .get_or_insert_with(|| cursor.target.clone());
                        d
                    }));
                }
            }
            out.extend(self.stored_relationship_diagnostics(path, id, data, &unit));
            out.extend(Self::data_model_attribute_diagnostics(path, id, data));
        }
        // The data models of the unit's other policies: what they read, so
        // a loop through them is found here too.
        for entry in unit.data_models.iter().filter(|e| e.policy_path != *path) {
            let Some(other) = self.raw_policy(&entry.policy_path) else {
                continue;
            };
            let Some(BlockDoc::DataModel { data, .. }) = other
                .blocks
                .iter()
                .find(|b| b.id() == Some(entry.block_id.as_ref()))
            else {
                continue;
            };
            for prop in &data.properties {
                for target in Self::host_targets(prop)
                    .into_iter()
                    .filter(|t| Self::orders(prop, t))
                {
                    let Some(source) = data.expression(&target).filter(|s| !s.trim().is_empty())
                    else {
                        continue;
                    };
                    let mut reads = Vec::new();
                    let deps = intellisense.borrow_mut().reads(&source);
                    ReadFlattener::extend_from_deps(&deps, &None, &mut reads);
                    for read in reads.iter().filter(|read| !read.via_alias) {
                        depends.extend(
                            Self::derivation_reads(&unit, &data.name, &read.path)
                                .into_iter()
                                .map(|to| Depend {
                                    from: Arc::from(format!("{}.{}", data.name, prop.name)),
                                    to,
                                    at: None,
                                }),
                        );
                    }
                }
            }
        }
        out.extend(Self::derivation_cycles(path, &depends));
        out
    }

    /// The expressions on a property: a feature's, a compute's and a call's.
    fn host_targets(prop: &PropertyDoc) -> Vec<CursorTarget> {
        let mut targets = Vec::new();
        if prop.feature.is_some() {
            targets.push(CursorTarget::FeatureExpr {
                id: prop.id.clone(),
            });
        }
        if prop.compute.is_some() {
            targets.push(CursorTarget::ComputeExpr {
                id: prop.id.clone(),
            });
        }
        if let Some(model) = &prop.model {
            let inputs = call_inputs(model);
            if let Some(inputs) = inputs.and_then(serde_json::Value::as_object) {
                targets.extend(inputs.keys().map(|input| CursorTarget::ModelInput {
                    id: prop.id.clone(),
                    input: Arc::from(input.as_str()),
                }));
            }
            if model.get("when").is_some() {
                targets.push(CursorTarget::ModelWhen {
                    id: prop.id.clone(),
                });
            }
            if model.get("request").is_some() && inputs.is_none() {
                targets.push(CursorTarget::ModelRequest {
                    id: prop.id.clone(),
                });
            }
            if model.get("response").is_some() {
                targets.push(CursorTarget::ModelResponse {
                    id: prop.id.clone(),
                });
            }
        }
        targets
    }

    /// Derived features and computes: null in, null out.
    fn propagates_null(prop: &PropertyDoc, target: &CursorTarget) -> bool {
        match target {
            CursorTarget::ComputeExpr { .. } => true,
            CursorTarget::FeatureExpr { .. } => prop
                .feature
                .as_ref()
                .is_some_and(|f| f.window.as_ref().is_none_or(|w| w.list().is_empty())),
            _ => false,
        }
    }

    /// Whether what an expression reads must be there before the property:
    /// derived features, computes and calls (their request, `when` and
    /// response; a call with named inputs reads the request, not the entity).
    fn orders(prop: &PropertyDoc, target: &CursorTarget) -> bool {
        Self::propagates_null(prop, target)
            || match target {
                CursorTarget::ModelRequest { .. } | CursorTarget::ModelResponse { .. } => true,
                CursorTarget::ModelWhen { .. } => prop
                    .model
                    .as_ref()
                    .is_some_and(|m| call_inputs(m).is_none()),
                _ => false,
            }
    }

    /// The fields a read from `entity` needs, as `entity.field`: each step
    /// through a reference or relationship (`card.txn_count` needs
    /// `transaction.card` and `card.txn_count`), and `$root.<entity>.…` from
    /// that entity.
    fn derivation_reads(unit: &Unit, entity: &str, path: &str) -> Vec<Arc<str>> {
        let segments: Vec<&str> = path.split('.').collect();
        let (mut entity, mut rest) = match segments.split_first() {
            Some((&"$root", rest)) => match rest.split_first() {
                Some((root, rest)) => (*root, rest),
                None => return Vec::new(),
            },
            _ => (entity, segments.as_slice()),
        };
        let mut out = Vec::new();
        while let Some((field, tail)) = rest.split_first() {
            let Some(prop) = unit
                .entities
                .get(entity)
                .and_then(|dm| dm.properties.iter().find(|p| p.name.as_ref() == *field))
            else {
                break;
            };
            out.push(Arc::from(format!("{entity}.{field}")));
            match &prop.kind {
                PropertyTypeIr::Reference { target } | PropertyTypeIr::Relationship { target }
                    if !tail.is_empty() && !prop.is_stored() =>
                {
                    entity = target.as_ref();
                    rest = tail;
                }
                _ => break,
            }
        }
        out
    }

    /// A feature's shape against its property: a list (`topK`, `lastN`,
    /// `unique`) needs `array: true`, a single value can't have it. A
    /// grouped feature (a map, on an `object` property) is either.
    fn feature_shape(
        path: &Arc<str>,
        block_id: &Arc<str>,
        prop: &PropertyDoc,
        source: &Arc<str>,
        scope: &VariableType,
        intellisense: &SharedIntelliSense,
    ) -> Option<Diagnostic> {
        use crate::policy::raw::PropertyTypeDoc;
        use zen_expression::parser::Node;
        const LISTS: [&str; 3] = ["topK", "lastN", "unique"];
        const SINGLES: [&str; 19] = [
            "sum",
            "count",
            "avg",
            "min",
            "max",
            "median",
            "mode",
            "stddev",
            "variance",
            "percentile",
            "countDistinct",
            "first",
            "last",
            "argMax",
            "argMin",
            "skew",
            "kurtosis",
            "countDistinctApprox",
            "percentileApprox",
        ];
        // A selector that gives the whole event (`argMax(t, t.amount)`,
        // `first(t, cond)`) is no feature: a feature is a value. Same rule
        // and wording as the feature store's.
        let whole_event = intellisense
            .borrow_mut()
            .with_ast(source, false, |root, _| {
                let mut node = root;
                while let Node::Parenthesized(inner) = node {
                    node = inner;
                }
                let Node::FunctionCall { kind, arguments } = node else {
                    return None;
                };
                let name = kind.to_string();
                // A plain column of the event: `t.merchant`, `#.amount`.
                let column = |node: &Node| {
                    let Node::Closure { body, .. } = node else {
                        return false;
                    };
                    let mut node = *body;
                    let mut steps = 0;
                    while let Node::Member { node: inner, .. } = node {
                        node = inner;
                        steps += 1;
                    }
                    steps > 0 && matches!(node, Node::Identifier(_) | Node::Pointer)
                };
                match (name.as_str(), arguments.len()) {
                    ("argMax" | "argMin", 2) => Some(format!(
                        "`{name}(events, by)` gives the whole event; a feature is a value: name what to return before the column to rank by: `{name}(transactions as t, t.merchant, t.amount [, cond])`"
                    )),
                    ("first" | "last", 1) => Some(name),
                    ("first" | "last", 2) if !column(arguments[1]) => Some(name),
                    _ => None,
                }
            })
            .flatten();
        if let Some(found) = whole_event {
            let message = if found.starts_with('`') {
                found
            } else {
                format!(
                    "`{found}` here gives the whole event; a feature is a value: name what to return, e.g. `{found}(transaction as t, t.merchant [, <condition>])`"
                )
            };
            return Some(Diagnostic::error(
                DiagnosticCode::FeatureShape,
                DiagnosticLocation::expression(path.clone(), block_id.clone(), prop.id.clone(), None)
                    .with_target(CursorTarget::FeatureExpr {
                        id: prop.id.clone(),
                    }),
                message,
            ));
        }
        if matches!(prop.property_type, PropertyTypeDoc::Object) {
            return None;
        }
        let aggregate = intellisense
            .borrow_mut()
            .with_ast(source, false, |root, _| {
                let mut node = root;
                while let Node::Parenthesized(inner) = node {
                    node = inner;
                }
                match node {
                    Node::FunctionCall { kind, .. } => Some(kind.to_string()),
                    _ => None,
                }
            })
            .flatten();
        let (list, shown) = match aggregate.as_deref() {
            Some(name) if LISTS.contains(&name) => (true, format!(" (`{name}`)")),
            Some(name) if SINGLES.contains(&name) => (false, format!(" (`{name}`)")),
            // Derived: by its type, when known.
            _ => {
                let returns = IntelliSenseSource::analyze(
                    &mut intellisense.borrow_mut(),
                    source,
                    ExpressionKind::Standard,
                    scope,
                )
                .return_type
                .shallow_clone();
                let returns = match returns {
                    VariableType::Nullable(inner) => inner.as_ref().shallow_clone(),
                    other => other,
                };
                match returns {
                    VariableType::Array(_) => (true, String::new()),
                    VariableType::Number
                    | VariableType::String
                    | VariableType::Bool
                    | VariableType::Date
                    | VariableType::Enum(..) => (false, String::new()),
                    _ => return None,
                }
            }
        };
        let message = match (list, prop.array) {
            (true, false) => format!(
                "`{}` is a list{shown}: set `array: true` on the property (its `type` is the items')",
                prop.name
            ),
            (false, true) => format!(
                "`{}` is a single value{shown}: remove `array: true` from the property",
                prop.name
            ),
            _ => return None,
        };
        Some(Diagnostic::error(
            DiagnosticCode::FeatureShape,
            DiagnosticLocation::expression(path.clone(), block_id.clone(), prop.id.clone(), None)
                .with_target(CursorTarget::FeatureExpr {
                    id: prop.id.clone(),
                }),
            message,
        ))
    }

    /// Attributes on a data model checked as written: old type names, the
    /// durations of calls, and attributes that another one overrides.
    fn data_model_attribute_diagnostics(
        path: &Arc<str>,
        id: &Arc<str>,
        data: &crate::policy::raw::DataModelDoc,
    ) -> Vec<Diagnostic> {
        use crate::policy::raw::PropertyTypeDoc;
        let mut out = Vec::new();
        let at = |prop: &PropertyDoc| {
            DiagnosticLocation::expression(path.clone(), id.clone(), prop.id.clone(), None)
        };
        let present =
            |value: &Option<serde_json::Value>| value.as_ref().is_some_and(|v| !v.is_null());
        if present(&data.events) && present(&data.reference) {
            out.push(Diagnostic::warning(
                DiagnosticCode::IgnoredAttribute,
                DiagnosticLocation::block(path.clone(), id.clone()),
                format!("`{}` has both `events` and `reference`: its records are events, `reference` is ignored", data.name),
            ));
        }
        // Old type names, still read: `decimal`/`integer` are numbers,
        // `timestamp` a date.
        for prop in &data.properties {
            let (old, new) = match &prop.property_type {
                PropertyTypeDoc::Decimal => ("decimal", "number"),
                PropertyTypeDoc::Integer => ("integer", "number"),
                PropertyTypeDoc::Timestamp => ("timestamp", "date"),
                _ => continue,
            };
            out.push(Diagnostic::warning(
                DiagnosticCode::DeprecatedType,
                at(prop),
                format!("`{}`: `{old}` is an old type name; use `{new}`", prop.name),
            ));
        }
        // How long a call's reply may be reused (`maxStaleness`: 5m) and
        // how long the call may take (`timeout`: 500ms).
        const STALENESS: (&str, &[&str], &str) =
            ("maxStaleness", &["s", "m", "h", "d"], "30s, 5m or 1h");
        const TIMEOUT: (&str, &[&str], &str) =
            ("timeout", &["ms", "s", "m", "h", "d"], "500ms, 2s or 1m");
        for prop in &data.properties {
            let Some(model) = prop.model.as_ref() else {
                continue;
            };
            if call_inputs(model).is_some() && model.get("request").is_some_and(|r| !r.is_null()) {
                out.push(Diagnostic::warning(
                    DiagnosticCode::IgnoredAttribute,
                    at(prop),
                    format!(
                        "call '{}' has both `inputs` and `request`: it sends the named `inputs`, `request` is ignored",
                        prop.name
                    ),
                ));
            }
            for (key, units, like) in [STALENESS, TIMEOUT] {
                let Some(duration) = model.get(key) else {
                    continue;
                };
                let text = duration.as_str().unwrap_or_default();
                // Empty means not set; anything but a duration string is wrong.
                if duration.is_null()
                    || (duration.is_string() && (text.is_empty() || is_duration(text, units)))
                {
                    continue;
                }
                out.push(Diagnostic::error(
                    DiagnosticCode::InvalidDuration,
                    at(prop),
                    format!(
                        "`{key}` of call '{}' is a duration like {like}, not `{}`",
                        prop.name,
                        if duration.is_string() {
                            text.to_string()
                        } else {
                            duration.to_string()
                        }
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
                    DiagnosticCode::InvalidRelationship,
                    DiagnosticLocation::expression(path.clone(), block_id.clone(), prop.id.clone(), None),
                    format!("`{}`: {message}", prop.name),
                ));
            };
            if on.is_some() && through.is_some() {
                problem("a stored relationship is found by key (`on`) or through events (`through`), not both".into());
                continue;
            }
            let Some(target_ir) = unit.entities.get(target) else {
                problem(match target.is_empty() {
                    true => "`target` is missing".to_string(),
                    false => format!("`{target}` isn't an entity"),
                });
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
                if events.is_empty() {
                    problem("`through.events` is missing".into());
                } else if events_ir.is_none_or(|e| e.records != Records::Events) {
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
                        if field.is_empty() {
                            problem(format!("`through.{role}` is missing: it names a reference of `{}` to `{expected}`", events_ir.name));
                        } else if reference_to(field).as_deref() != Some(expected) {
                            problem(format!(
                                "`{role}` is `{field}`: it names a reference of `{}` to `{expected}`",
                                events_ir.name
                            ));
                        }
                    }
                }
                if text("window").is_none_or(str::is_empty) {
                    problem("`through.window` is missing: a duration like 10m, 1h or 30d".into());
                } else if !text("window").is_some_and(|w| is_duration(w, &["m", "h", "d"])) {
                    problem(format!(
                        "`window` is a duration like 10m, 1h or 30d, not `{}`",
                        text("window").unwrap_or_default()
                    ));
                }
            }
        }
        out
    }

    /// A derived field that needs its own value, directly (`x = x + 5`) or
    /// through others (`a = b`, `b = a`), in its entity or through another
    /// (`transaction.risk_score` → `card.card_score` → back): no order
    /// computes it. Reported on each read in this policy that closes the loop.
    fn derivation_cycles(path: &Arc<str>, depends: &[Depend]) -> Vec<Diagnostic> {
        let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
        for depend in depends {
            edges
                .entry(depend.from.as_ref())
                .or_default()
                .push(depend.to.as_ref());
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
        let split = |node: &str| -> (String, String) {
            let (entity, field) = node.split_once('.').unwrap_or(("", node));
            (entity.to_string(), field.to_string())
        };
        let mut out = Vec::new();
        for Depend { from, to, at } in depends {
            let Some(at) = at else {
                continue;
            };
            let (entity, field) = split(from);
            let message = if from == to {
                format!("`{field}` reads itself: it can't be computed from its own value")
            } else if let Some(trail) = way_back(to, from) {
                // Within one entity by field name; across entities qualified.
                let within = trail.iter().all(|node| split(node).0 == entity);
                let show = |node: &str| {
                    if within {
                        split(node).1
                    } else {
                        node.to_string()
                    }
                };
                format!(
                    "`{}` depends on itself: {} → {}",
                    show(from),
                    show(from),
                    trail
                        .iter()
                        .map(|node| show(node))
                        .collect::<Vec<_>>()
                        .join(" → ")
                )
            } else {
                continue;
            };
            out.push(Diagnostic::error(
                DiagnosticCode::CyclicDependency,
                DiagnosticLocation::expression(
                    path.clone(),
                    at.block_id.clone(),
                    at.prop_id.clone(),
                    at.span,
                )
                .with_target(at.target.clone()),
                message,
            ));
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
        use crate::workspace::types::Severity;
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
            // Data model errors (a derivation cycle, a bad `maxStaleness`, a
            // stored relationship that can't be found) stop a compile too, so
            // hosts don't repeat these checks.
            candidates.extend(
                self.data_model_expression_diagnostics(member)
                    .into_iter()
                    .filter(|d| d.severity == Severity::Error),
            );
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

/// A derived field or call (`from`) needing another field (`to`), both
/// `entity.field`; `at` where this policy reads it (else another's).
struct Depend {
    from: Arc<str>,
    to: Arc<str>,
    at: Option<DependSite>,
}

struct DependSite {
    block_id: Arc<str>,
    target: CursorTarget,
    prop_id: Arc<str>,
    span: Option<(u32, u32)>,
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

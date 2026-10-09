use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ahash::HashMap;

use zen_expression::variable::VariableType;
use zen_types::decision::{
    DecisionNodeKind, DecisionTableContent, DecisionTableHitPolicy, OutputNodeContent,
    TransformAttributes, TransformExecutionMode,
};

use super::siblings::SiblingCache;
use super::CursorScope;
use crate::model::GraphContent;
use crate::policy::blocks::{
    BlockKind, DecisionTableIr, IntelliSenseSource, SharedIntelliSense, ROW_ID_KEY,
};
use crate::policy::ir::{DataModelIr, PropertyTypeIr, Records};
use crate::policy::raw::BlockDoc;
use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::db::{Db, Unit};
use crate::workspace::graph::{GraphAnalyzer, GraphNodeAnalysis, SchemaType};
use crate::workspace::types::{BlockRef, Cursor, CursorTarget, ExpressionKind};

impl Db {
    pub(super) fn policy_cursor_scope(
        &self,
        cursor: &Cursor,
        cache: &mut SiblingCache,
    ) -> Option<CursorScope> {
        let block_ref = BlockRef {
            policy_path: cursor.policy_path.clone(),
            block_id: cursor.block_id.clone(),
        };
        let block = self.block_ir(&block_ref)?;
        let unit = self.unit(&cursor.policy_path);
        let enriched = self.enriched_of_unit(&unit);
        let scope = enriched.scope_excluding(&block_ref);
        match (&block.kind, &cursor.target) {
            (BlockKind::DecisionTable(table), _) => {
                self.policy_table_scope(&unit, table, cursor, scope, &enriched.scope, cache)
            }
            (BlockKind::Expression(_), CursorTarget::ExpressionKey { .. })
            | (BlockKind::Assertion(_), CursorTarget::AssertionOutput)
            | (BlockKind::Match(_), CursorTarget::MatchTarget) => Some(CursorScope::path(scope)),
            (BlockKind::Expression(expression), CursorTarget::Expression { .. }) => Some(
                CursorScope::value(scope, self.declared_type(&unit, &expression.key)),
            ),
            (BlockKind::Match(block), CursorTarget::MatchValue { .. }) => Some(CursorScope::value(
                scope,
                self.declared_type(&unit, &block.key),
            )),
            (BlockKind::Assertion(_) | BlockKind::Match(_), CursorTarget::Expression { .. }) => {
                Some(CursorScope::condition(scope))
            }
            _ => None,
        }
    }

    /// Expressions on a data model property, read by the host rather than
    /// run by the policy. A windowed feature aggregates the records of an
    /// events (or reference) entity: `count(transaction)`. A derived feature
    /// and a per-event `compute` read the entity's own properties (and
    /// through references, other entities' features). A model's inputs and
    /// `when` read the request.
    pub(crate) fn data_model_cursor_scope(&self, cursor: &Cursor) -> Option<CursorScope> {
        let unit = self.unit(&cursor.policy_path);
        let entry = unit
            .data_models
            .iter()
            .find(|e| e.policy_path == cursor.policy_path && e.block_id == cursor.block_id)?;
        let enriched = self.enriched_of_unit(&unit);
        let own = || -> HashMap<Rc<str>, VariableType> {
            match enriched.declared_at(&entry.ir.name) {
                VariableType::Object(obj) => obj.borrow().clone(),
                _ => HashMap::default(),
            }
        };
        let object = |fields: HashMap<Rc<str>, VariableType>| {
            VariableType::Object(Rc::new(RefCell::new(fields)))
        };
        match &cursor.target {
            CursorTarget::FeatureExpr { id } => {
                let BlockDoc::DataModel { data, .. } = self.block_doc(&BlockRef {
                    policy_path: cursor.policy_path.clone(),
                    block_id: cursor.block_id.clone(),
                })?
                else {
                    return None;
                };
                let feature = data.properties.iter().find(|p| p.id == *id)?.feature.as_ref()?;
                let windowed = feature.window.as_ref().is_some_and(|w| !w.list().is_empty());
                let mut fields = if windowed {
                    let mut sources: Vec<&Arc<DataModelIr>> = unit
                        .entities
                        .values()
                        .filter(|dm| dm.records != Records::Plain)
                        .collect();
                    if sources.is_empty() {
                        sources = unit.entities.values().collect();
                    }
                    sources
                        .into_iter()
                        .map(|dm| {
                            (
                                Rc::from(dm.name.as_ref()),
                                enriched.declared_at(&dm.name).array(),
                            )
                        })
                        .collect()
                } else {
                    // Derived at read time: the instant read for.
                    let mut fields = own();
                    fields.entry(Rc::from("asOf")).or_insert(VariableType::Date);
                    fields
                };
                // A name the scope has already is the real one: never shadowed.
                fields
                    .entry(Rc::from("params"))
                    .or_insert_with(|| self.feature_params(&unit));
                Some(CursorScope::value(object(fields), None))
            }
            CursorTarget::ComputeExpr { .. } => Some(CursorScope::value(object(own()), None)),
            CursorTarget::ModelInput { .. } => {
                Some(CursorScope::value(enriched.declared_scope(), None))
            }
            // What a call sends: the instance, as its derived features read
            // it; `$root` the whole request (a parent the instance can't reach).
            CursorTarget::ModelRequest { .. } => {
                let mut fields = own();
                fields.insert(Rc::from("$root"), enriched.declared_scope());
                Some(CursorScope::value(object(fields), None))
            }
            // The call's value: the reply (`response`) beside the instance.
            CursorTarget::ModelResponse { .. } => {
                let mut fields = own();
                fields.entry(Rc::from("response")).or_insert(VariableType::Any);
                fields.insert(Rc::from("$root"), enriched.declared_scope());
                Some(CursorScope::value(object(fields), None))
            }
            // `when` reads the instance; a call written with named `inputs`
            // (the earlier format) reads the request and those names.
            CursorTarget::ModelWhen { id } => {
                let BlockDoc::DataModel { data, .. } = self.block_doc(&BlockRef {
                    policy_path: cursor.policy_path.clone(),
                    block_id: cursor.block_id.clone(),
                })?
                else {
                    return None;
                };
                let model = data.properties.iter().find(|p| p.id == *id)?.model.as_ref()?;
                if crate::policy::raw::call_inputs(model).is_some() {
                    Some(CursorScope::condition(enriched.declared_scope()))
                } else {
                    let mut fields = own();
                    fields.insert(Rc::from("$root"), enriched.declared_scope());
                    Some(CursorScope::condition(object(fields)))
                }
            }
            _ => None,
        }
    }

    /// `params` in feature expressions: the constants of a `featureSettings`
    /// block in the policy or its imports (`{ name, type, value }`).
    fn feature_params(&self, unit: &Unit) -> VariableType {
        let mut fields: HashMap<Rc<str>, VariableType> = HashMap::default();
        let mut members: Vec<&Arc<str>> = unit.members.iter().collect();
        members.sort();
        for member in members {
            let Some(policy) = self.raw_policy(member) else {
                continue;
            };
            for block in &policy.blocks {
                let BlockDoc::Ignored(value) = block else {
                    continue;
                };
                if value.get("type").and_then(serde_json::Value::as_str) != Some("featureSettings") {
                    continue;
                }
                let params = value
                    .pointer("/props/data/params")
                    .and_then(serde_json::Value::as_array);
                for param in params.into_iter().flatten() {
                    let Some(name) = param.get("name").and_then(serde_json::Value::as_str) else {
                        continue;
                    };
                    let kind = match param.get("type").and_then(serde_json::Value::as_str) {
                        Some("number" | "decimal" | "integer") => VariableType::Number,
                        Some("string") => VariableType::String,
                        Some("boolean") => VariableType::Bool,
                        Some("date" | "timestamp") => VariableType::Date,
                        _ => VariableType::Any,
                    };
                    fields.entry(Rc::from(name)).or_insert(kind);
                }
            }
        }
        VariableType::Object(Rc::new(RefCell::new(fields)))
    }

    fn policy_table_scope(
        &self,
        unit: &Unit,
        table: &DecisionTableIr,
        cursor: &Cursor,
        scope: VariableType,
        written: &VariableType,
        cache: &mut SiblingCache,
    ) -> Option<CursorScope> {
        match &cursor.target {
            CursorTarget::DecisionTableHead { col } => {
                if table.inputs.iter().any(|c| c.id == *col) {
                    return Some(CursorScope::value(scope, None));
                }
                table
                    .outputs
                    .iter()
                    .any(|c| c.id == *col)
                    .then(|| CursorScope::path(scope))
            }
            CursorTarget::DecisionTableCell { col, .. } => {
                if let Some(column) = table.inputs.iter().find(|c| c.id == *col) {
                    return Some(match column.field.as_ref().filter(|f| !f.is_empty()) {
                        Some(field) => {
                            let field_type = Self::return_type(&self.intellisense(), field, &scope);
                            CursorScope::unary(scope.with_dollar(&field_type))
                        }
                        None => CursorScope::condition(scope),
                    });
                }
                let column = table.outputs.iter().find(|c| c.id == *col)?;
                let expected = column
                    .declared
                    .as_ref()
                    .and_then(|declared| declared.resolve(&unit.dictionary_types()))
                    .or_else(|| {
                        self.declared_type(unit, column.field.as_ref())
                            .map(|t| CursorScope::collected_type(t, column.collect))
                    });
                // The table is the field's only writer, so its written type is its own cells' union.
                let inferred = expected.is_none();
                let expected = expected
                    .or_else(|| {
                        self.written_type(unit, written, column.field.as_ref())
                            .map(|t| CursorScope::collected_type(t, column.collect))
                    })
                    .or_else(|| {
                        let is = self.intellisense();
                        cache.infer(cursor, || {
                            table.rules.iter().filter_map(|rule| {
                                let id = rule.get(ROW_ID_KEY)?.clone();
                                let t = rule
                                    .get(col)
                                    .filter(|cell| !cell.is_empty())
                                    .map(|cell| Self::return_type(&is, cell, &scope));
                                Some((id, t))
                            })
                        })
                    });
                Some(CursorScope::value(scope, expected).with_inferred(inferred))
            }
            _ => None,
        }
    }

    fn declared_type(&self, unit: &Unit, path: &str) -> Option<VariableType> {
        if path.is_empty() {
            return None;
        }
        let kind = CursorScope::known_type(self.enriched_of_unit(unit).declared_at(path))?;
        let segments: Vec<Rc<str>> = path.split('.').map(Rc::from).collect();
        let (field, parent) = segments.split_last()?;
        let property = if parent.is_empty() {
            unit.entity_graph.global_property(field)
        } else {
            unit.entity_graph
                .resolve_path_to_element(parent)
                .and_then(|entity| unit.entities.get(&entity))
                .and_then(|model| {
                    model
                        .properties
                        .iter()
                        .find(|prop| prop.name.as_ref() == field.as_ref())
                })
        };
        Some(
            if property.is_some_and(|prop| matches!(prop.kind, PropertyTypeIr::Date)) {
                CursorScope::date_hint(kind)
            } else {
                kind
            },
        )
    }

    fn written_type(&self, unit: &Unit, scope: &VariableType, path: &str) -> Option<VariableType> {
        if path.is_empty() {
            return None;
        }
        self.declared_type(unit, path)
            .or_else(|| CursorScope::literal_union(scope.resolve_at(path)))
    }

    pub(super) fn graph_cursor_scope(
        &self,
        cursor: &Cursor,
        cache: &mut SiblingCache,
    ) -> Option<CursorScope> {
        let snap = self.snapshot();
        let doc = snap.graphs.get(&cursor.policy_path)?.clone();
        let content = doc.as_graph()?;
        let analysis = self.graph_analysis(&cursor.policy_path)?;
        let node = content.nodes.iter().find(|n| n.id == cursor.block_id)?;
        let node_analysis = analysis.nodes.get(&cursor.block_id)?;
        let input_scope =
            || GraphAnalyzer::scope_with_nodes(&node_analysis.input, &node_analysis.nodes_scope);

        match (&node.kind, &cursor.target) {
            (
                DecisionNodeKind::ExpressionNode { .. }
                | DecisionNodeKind::DecisionTableNode { .. }
                | DecisionNodeKind::DecisionNode { .. },
                CursorTarget::TransformInput,
            ) => Some(CursorScope::value(input_scope(), None)),
            (DecisionNodeKind::ExpressionNode { .. }, CursorTarget::ExpressionKey { .. }) => {
                Some(CursorScope::path(input_scope()))
            }
            (
                DecisionNodeKind::ExpressionNode { content: rows },
                CursorTarget::Expression { id },
            ) => {
                let scope = GraphAnalyzer::scope_with(
                    &node_analysis.handler_input,
                    &[
                        ("$", node_analysis.dollar_before(&rows.expressions, id)),
                        ("$nodes", node_analysis.nodes_scope.shallow_clone()),
                    ],
                );
                let expected = rows
                    .expressions
                    .iter()
                    .find(|row| row.id == *id)
                    .filter(|row| !row.key.is_empty())
                    .and_then(|row| {
                        self.output_schema_type(
                            content,
                            &cursor.block_id,
                            &row.key,
                            rows.transform_attributes.output_path.as_deref(),
                        )
                    });
                Some(CursorScope::value(scope, expected))
            }
            (DecisionNodeKind::SwitchNode { .. }, CursorTarget::Expression { .. }) => {
                Some(CursorScope::condition(input_scope()))
            }
            (DecisionNodeKind::DecisionTableNode { content: table }, _) => {
                self.graph_table_scope(content, table, node_analysis, cursor, cache)
            }
            _ => None,
        }
    }

    fn graph_table_scope(
        &self,
        content: &GraphContent,
        table: &DecisionTableContent,
        node_analysis: &GraphNodeAnalysis,
        cursor: &Cursor,
        cache: &mut SiblingCache,
    ) -> Option<CursorScope> {
        let scope = GraphAnalyzer::scope_with_nodes(
            &node_analysis.handler_input,
            &node_analysis.nodes_scope,
        );
        match &cursor.target {
            CursorTarget::DecisionTableHead { col } => {
                if table.inputs.iter().any(|c| c.id == *col) {
                    return Some(CursorScope::value(scope, None));
                }
                table
                    .outputs
                    .iter()
                    .any(|c| c.id == *col)
                    .then(|| CursorScope::path(scope))
            }
            CursorTarget::DecisionTableCell { col, .. } => {
                let is = self.graph_intellisense();
                if let Some(column) = table.inputs.iter().find(|c| c.id == *col) {
                    return Some(match column.field.as_ref().filter(|f| !f.is_empty()) {
                        Some(field) => {
                            let field_type = Self::return_type(&is, field, &scope);
                            CursorScope::unary(scope.with_dollar(&field_type))
                        }
                        None => CursorScope::condition(scope),
                    });
                }
                let column = table.outputs.iter().find(|c| c.id == *col)?;
                let dictionaries = self.graph_dictionary_types(content);
                let expected =
                    GraphAnalyzer::output_expected(table, col, &dictionaries).or_else(|| {
                        (!column.field.is_empty())
                            .then(|| {
                                self.output_schema_type(
                                    content,
                                    &cursor.block_id,
                                    column.field.strip_suffix("[]").unwrap_or(&column.field),
                                    table.transform_attributes.output_path.as_deref(),
                                )
                                .map(|t| {
                                    CursorScope::collected_type(t, column.field.ends_with("[]"))
                                })
                            })
                            .flatten()
                    });
                let inferred = expected.is_none();
                let expected = expected.or_else(|| {
                    cache.infer(cursor, || {
                        table.rules.iter().enumerate().map(|(index, rule)| {
                            let id = GraphAnalyzer::row_key(rule, index);
                            let t = rule
                                .get(col)
                                .filter(|cell| !cell.is_empty())
                                .map(|cell| Self::return_type(&is, cell, &scope));
                            (id, t)
                        })
                    })
                });
                Some(CursorScope::value(scope, expected).with_inferred(inferred))
            }
            _ => None,
        }
    }

    fn output_schema_type(
        &self,
        content: &GraphContent,
        node_id: &str,
        key: &str,
        output_path: Option<&str>,
    ) -> Option<VariableType> {
        let dictionaries = self.graph_dictionary_types(content);
        let key = match output_path.filter(|p| !p.is_empty()) {
            Some(path) => format!("{path}.{key}"),
            None => key.to_string(),
        };
        let mut resolved: Option<VariableType> = None;
        for output in Self::reachable_outputs(content, node_id) {
            let schema = output.schema.as_ref()?;
            let found = CursorScope::known_type(
                SchemaType::variable_type_with(schema, &dictionaries).resolve_at(&key),
            )?;
            resolved = match resolved {
                None => Some(found),
                Some(existing) => {
                    let (left, _) = existing.unwrap_nullable();
                    let (right, _) = found.unwrap_nullable();
                    if !(left.satisfies(right) && right.satisfies(left)) {
                        return None;
                    }
                    let merged = existing.merge(&found);
                    match existing.is_nullable() && found.is_nullable() {
                        true => Some(merged),
                        false => Some(merged.unwrap_nullable().0.shallow_clone()),
                    }
                }
            };
        }
        resolved
    }

    fn reachable_outputs<'c>(
        content: &'c GraphContent,
        node_id: &'c str,
    ) -> Vec<&'c OutputNodeContent> {
        let mut visited: Vec<&str> = vec![node_id];
        let mut queue: Vec<&str> = vec![node_id];
        let mut outputs = Vec::new();
        while let Some(current) = queue.pop() {
            for edge in content
                .edges
                .iter()
                .filter(|e| e.source_id.as_ref() == current)
            {
                let Some(target) = content.nodes.iter().find(|n| n.id == edge.target_id) else {
                    continue;
                };
                let passes = match &target.kind {
                    DecisionNodeKind::OutputNode { content: output } => {
                        outputs.push(output);
                        false
                    }
                    DecisionNodeKind::SwitchNode { .. } => true,
                    DecisionNodeKind::ExpressionNode { content: node } => {
                        Self::passes_through(&node.transform_attributes, false)
                    }
                    DecisionNodeKind::DecisionNode { content: node } => {
                        Self::passes_through(&node.transform_attributes, false)
                    }
                    DecisionNodeKind::DecisionTableNode { content: node } => Self::passes_through(
                        &node.transform_attributes,
                        node.hit_policy == DecisionTableHitPolicy::Collect,
                    ),
                    _ => false,
                };
                if passes && !visited.contains(&target.id.as_ref()) {
                    visited.push(target.id.as_ref());
                    queue.push(target.id.as_ref());
                }
            }
        }
        outputs
    }

    fn passes_through(attributes: &TransformAttributes, collects: bool) -> bool {
        attributes.pass_through
            && (attributes.output_path.is_some()
                || (!collects && attributes.execution_mode == TransformExecutionMode::Single))
    }

    fn return_type(
        is: &SharedIntelliSense,
        source: &Arc<str>,
        scope: &VariableType,
    ) -> VariableType {
        IntelliSenseSource::analyze(
            &mut is.borrow_mut(),
            source,
            ExpressionKind::Standard,
            scope,
        )
        .return_type
        .shallow_clone()
    }
}

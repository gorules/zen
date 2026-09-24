use std::rc::Rc;
use std::sync::Arc;

use zen_expression::variable::VariableType;
use zen_types::decision::{DecisionNodeKind, DecisionTableContent};

use super::siblings::SiblingCache;
use super::CursorScope;
use crate::model::GraphContent;
use crate::policy::blocks::{
    BlockKind, DecisionTableIr, IntelliSenseSource, SharedIntelliSense, ROW_ID_KEY,
};
use crate::policy::ir::PropertyTypeIr;
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
                let known = table.inputs.iter().any(|c| c.id == *col)
                    || table.outputs.iter().any(|c| c.id == *col);
                known.then(|| CursorScope::path(scope))
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
                        self.written_type(unit, written, column.field.as_ref())
                            .map(|t| CursorScope::collected_type(t, column.collect))
                    })
                    .or_else(|| {
                        cache.infer(cursor, || {
                            let is = self.intellisense();
                            table
                                .rules
                                .iter()
                                .filter_map(|rule| {
                                    let id = rule.get(ROW_ID_KEY)?.clone();
                                    let t = rule
                                        .get(col)
                                        .filter(|cell| !cell.is_empty())
                                        .map(|cell| Self::return_type(&is, cell, &scope));
                                    Some((id, t))
                                })
                                .collect()
                        })
                    });
                Some(CursorScope::value(scope, expected))
            }
            _ => None,
        }
    }

    fn declared_type(&self, unit: &Unit, path: &str) -> Option<VariableType> {
        if path.is_empty() {
            return None;
        }
        let kind = CursorScope::known_type(self.snapshot().base_scope.resolve_at(path))?;
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
            )
            | (DecisionNodeKind::ExpressionNode { .. }, CursorTarget::ExpressionKey { .. }) => {
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
                let known = table.inputs.iter().any(|c| c.id == *col)
                    || table.outputs.iter().any(|c| c.id == *col);
                known.then(|| CursorScope::path(scope))
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
                let dictionaries = self.graph_dictionary_types(&content.imports);
                let expected = GraphAnalyzer::output_expected(table, col, &dictionaries)
                    .or_else(|| {
                        (!column.field.is_empty())
                            .then(|| {
                                self.output_schema_type(
                                    content,
                                    column.field.strip_suffix("[]").unwrap_or(&column.field),
                                    table.transform_attributes.output_path.as_deref(),
                                )
                                .map(|t| {
                                    CursorScope::collected_type(t, column.field.ends_with("[]"))
                                })
                            })
                            .flatten()
                    })
                    .or_else(|| {
                        cache.infer(cursor, || {
                            table
                                .rules
                                .iter()
                                .enumerate()
                                .map(|(index, rule)| {
                                    let id = GraphAnalyzer::row_key(rule, index);
                                    let t = rule
                                        .get(col)
                                        .filter(|cell| !cell.is_empty())
                                        .map(|cell| Self::return_type(&is, cell, &scope));
                                    (id, t)
                                })
                                .collect()
                        })
                    });
                Some(CursorScope::value(scope, expected))
            }
            _ => None,
        }
    }

    fn output_schema_type(
        &self,
        content: &GraphContent,
        key: &str,
        output_path: Option<&str>,
    ) -> Option<VariableType> {
        let schema = content.nodes.iter().find_map(|node| match &node.kind {
            DecisionNodeKind::OutputNode { content } => content.schema.as_ref(),
            _ => None,
        })?;
        let dictionaries = self.graph_dictionary_types(&content.imports);
        let output = SchemaType::hint_type_with(schema, &dictionaries);
        let key = match output_path.filter(|p| !p.is_empty()) {
            Some(path) => format!("{path}.{key}"),
            None => key.to_string(),
        };
        CursorScope::known_type(output.resolve_at(&key))
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

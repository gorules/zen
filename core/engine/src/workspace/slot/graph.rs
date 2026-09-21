use std::sync::Arc;

use zen_expression::variable::VariableType;
use zen_types::decision::{DecisionNode, DecisionNodeKind, DecisionTableContent};

use super::siblings::SiblingCache;
use super::{collected_type, date_hint, is_date_type, known_type, CursorScope};
use crate::model::GraphContent;
use crate::policy::blocks::IntelliSenseSource;
use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::db::Db;
use crate::workspace::graph::{GraphAnalyzer, GraphNodeAnalysis, SchemaType};
use crate::workspace::types::{Cursor, CursorTarget, ExpressionKind};

pub(super) fn graph_scope(
    db: &Db,
    cursor: &Cursor,
    cache: &mut SiblingCache,
) -> Option<CursorScope> {
    let snap = db.snapshot();
    let doc = snap.graphs.get(&cursor.policy_path)?.clone();
    let content = doc.as_graph()?;
    let analysis = db.graph_analysis(&cursor.policy_path)?;
    let node = content.nodes.iter().find(|n| n.id == cursor.block_id)?;
    let node_analysis = analysis.nodes.get(&cursor.block_id)?;

    if matches!(cursor.target, CursorTarget::TransformInput) {
        if !has_transform_attributes(node) {
            return None;
        }
        return Some(CursorScope::path(GraphAnalyzer::scope_with_nodes(
            &node_analysis.input,
            &node_analysis.nodes_scope,
        )));
    }

    match &node.kind {
        DecisionNodeKind::ExpressionNode { content: rows } => {
            if matches!(cursor.target, CursorTarget::ExpressionKey) {
                return Some(CursorScope::path(GraphAnalyzer::scope_with_nodes(
                    &node_analysis.input,
                    &node_analysis.nodes_scope,
                )));
            }
            let CursorTarget::Expression { id } = &cursor.target else {
                return None;
            };
            let dollar = node_analysis
                .dollar
                .clone()
                .unwrap_or_else(VariableType::empty_object);
            let scope = GraphAnalyzer::scope_with(
                &node_analysis.handler_input,
                &[
                    ("$", dollar),
                    ("$nodes", node_analysis.nodes_scope.shallow_clone()),
                ],
            );
            let expected = rows
                .expressions
                .iter()
                .find(|row| row.id == *id)
                .filter(|row| !row.key.is_empty())
                .and_then(|row| {
                    output_schema_type(
                        db,
                        content,
                        &row.key,
                        rows.transform_attributes.output_path.as_deref(),
                    )
                });
            Some(CursorScope::value(scope, expected))
        }
        DecisionNodeKind::SwitchNode { .. } => {
            let CursorTarget::Expression { .. } = &cursor.target else {
                return None;
            };
            Some(CursorScope::condition(GraphAnalyzer::scope_with_nodes(
                &node_analysis.input,
                &node_analysis.nodes_scope,
            )))
        }
        DecisionNodeKind::DecisionTableNode { content: table } => {
            table_scope(db, content, table, node_analysis, cursor, cache)
        }
        _ => None,
    }
}

fn table_scope(
    db: &Db,
    content: &GraphContent,
    table: &DecisionTableContent,
    node_analysis: &GraphNodeAnalysis,
    cursor: &Cursor,
    cache: &mut SiblingCache,
) -> Option<CursorScope> {
    let scope =
        GraphAnalyzer::scope_with_nodes(&node_analysis.handler_input, &node_analysis.nodes_scope);
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
                        let field_type = return_type(db, field, &scope);
                        let path = match table
                            .transform_attributes
                            .input_field
                            .as_deref()
                            .filter(|p| !p.is_empty())
                        {
                            Some(prefix) => format!("{prefix}.{field}"),
                            None => field.to_string(),
                        };
                        let hint = content
                            .nodes
                            .iter()
                            .find_map(|node| match &node.kind {
                                DecisionNodeKind::InputNode { content } => content.schema.as_ref(),
                                _ => None,
                            })
                            .filter(|schema| SchemaType::is_date_path(schema, &path))
                            .map(|_| date_hint(field_type.shallow_clone()))
                            .filter(is_date_type);
                        CursorScope::unary_with_hint(scope.with_dollar(&field_type), hint)
                    }
                    None => CursorScope::condition(scope),
                });
            }
            let column = table.outputs.iter().find(|c| c.id == *col)?;
            let dictionaries = db.graph_dictionary_types(&content.imports);
            let expected = GraphAnalyzer::output_expected(table, col, &dictionaries)
                .or_else(|| {
                    (!column.field.is_empty())
                        .then(|| {
                            output_schema_type(
                                db,
                                content,
                                column.field.strip_suffix("[]").unwrap_or(&column.field),
                                table.transform_attributes.output_path.as_deref(),
                            )
                            .map(|t| {
                                // The collect hit policy wraps rows, which resolve_at already
                                // traverses. Only a column marker wraps this field's value.
                                collected_type(t, column.field.ends_with("[]"))
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
                                    .map(|cell| return_type(db, cell, &scope));
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
    db: &Db,
    content: &GraphContent,
    key: &str,
    output_path: Option<&str>,
) -> Option<VariableType> {
    let schema = content.nodes.iter().find_map(|node| match &node.kind {
        DecisionNodeKind::OutputNode { content } => content.schema.as_ref(),
        _ => None,
    })?;
    let dictionaries = db.graph_dictionary_types(&content.imports);
    let output = SchemaType::hint_type_with(schema, &dictionaries);
    let key = match output_path.filter(|p| !p.is_empty()) {
        Some(path) => format!("{path}.{key}"),
        None => key.to_string(),
    };
    known_type(output.resolve_at(&key))
}

fn has_transform_attributes(node: &DecisionNode) -> bool {
    matches!(
        node.kind,
        DecisionNodeKind::ExpressionNode { .. }
            | DecisionNodeKind::DecisionTableNode { .. }
            | DecisionNodeKind::DecisionNode { .. }
    )
}

fn return_type(db: &Db, source: &Arc<str>, scope: &VariableType) -> VariableType {
    let intellisense = db.graph_intellisense();
    let mut is = intellisense.borrow_mut();
    IntelliSenseSource::analyze(&mut is, source, ExpressionKind::Standard, scope)
        .return_type
        .shallow_clone()
}

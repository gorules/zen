use std::sync::Arc;

use zen_expression::variable::VariableType;
use zen_types::decision::{DecisionNode, DecisionNodeKind, DecisionTableContent};

use crate::policy::blocks::IntelliSenseSource;
use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::db::Db;
use crate::workspace::graph::analysis::{GraphAnalyzer, GraphNodeAnalysis};
use crate::workspace::types::{Cursor, CursorTarget, ExpressionKind};

impl Db {
    pub(crate) fn graph_resolve_cursor(
        &self,
        cursor: &Cursor,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        let snap = self.snapshot();
        let doc = snap.graphs.get(&cursor.policy_path)?.clone();
        let content = doc.as_graph()?;
        let analysis = self.graph_analysis(&cursor.policy_path)?;
        let node = content.nodes.iter().find(|n| n.id == cursor.block_id)?;
        let node_analysis = analysis.nodes.get(&cursor.block_id)?;
        self.resolve_in_node(node, node_analysis, cursor)
    }

    pub(crate) fn resolve_in_node(
        &self,
        node: &DecisionNode,
        node_analysis: &GraphNodeAnalysis,
        cursor: &Cursor,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        if matches!(cursor.target, CursorTarget::TransformInput) {
            let attributes = super::editor::NodePaths::attributes(node)?;
            let field = attributes.input_field.as_ref()?;
            let scope =
                GraphAnalyzer::scope_with_nodes(&node_analysis.input, &node_analysis.nodes_scope);
            return Some((field.clone(), ExpressionKind::Standard, scope));
        }

        match &node.kind {
            DecisionNodeKind::ExpressionNode { content } => {
                let CursorTarget::Expression { id } = &cursor.target else {
                    return None;
                };
                let row = content.expressions.iter().find(|row| row.id == *id)?;
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
                Some((row.value.clone(), ExpressionKind::Standard, scope))
            }
            DecisionNodeKind::SwitchNode { content } => {
                let CursorTarget::Expression { id } = &cursor.target else {
                    return None;
                };
                let statement = content.statements.iter().find(|s| s.id == *id)?;
                let scope = GraphAnalyzer::scope_with_nodes(
                    &node_analysis.input,
                    &node_analysis.nodes_scope,
                );
                Some((statement.condition.clone(), ExpressionKind::Standard, scope))
            }
            DecisionNodeKind::DecisionTableNode { content } => {
                self.resolve_in_table(content, node_analysis, cursor)
            }
            _ => None,
        }
    }

    fn resolve_in_table(
        &self,
        content: &DecisionTableContent,
        node_analysis: &GraphNodeAnalysis,
        cursor: &Cursor,
    ) -> Option<(Arc<str>, ExpressionKind, VariableType)> {
        let base_scope = GraphAnalyzer::scope_with_nodes(
            &node_analysis.handler_input,
            &node_analysis.nodes_scope,
        );
        match &cursor.target {
            CursorTarget::DecisionTableHead { col } => {
                let column = content.inputs.iter().find(|c| c.id == *col)?;
                let field = column.field.as_ref()?;
                Some((field.clone(), ExpressionKind::Standard, base_scope))
            }
            CursorTarget::DecisionTableCell { row, col } => {
                let rule = content
                    .rules
                    .iter()
                    .enumerate()
                    .find(|(idx, rule)| GraphAnalyzer::row_key(rule, *idx) == *row)
                    .map(|(_, rule)| rule)?;
                let cell = rule.get(col).cloned().unwrap_or_else(|| Arc::from(""));
                let cell = &cell;

                if let Some(column) = content.inputs.iter().find(|c| c.id == *col) {
                    return match &column.field {
                        Some(field) => {
                            let intellisense = self.graph_intellisense();
                            let field_type = IntelliSenseSource::analyze(
                                &mut intellisense.borrow_mut(),
                                field,
                                ExpressionKind::Standard,
                                &base_scope,
                            )
                            .return_type
                            .shallow_clone();
                            Some((
                                cell.clone(),
                                ExpressionKind::Unary,
                                base_scope.with_dollar(&field_type),
                            ))
                        }
                        None => Some((cell.clone(), ExpressionKind::Standard, base_scope)),
                    };
                }
                content
                    .outputs
                    .iter()
                    .any(|c| c.id == *col)
                    .then(|| (cell.clone(), ExpressionKind::Standard, base_scope))
            }
            _ => None,
        }
    }
}

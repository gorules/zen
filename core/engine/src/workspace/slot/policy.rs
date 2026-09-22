use std::rc::Rc;
use std::sync::Arc;

use zen_expression::variable::VariableType;

use super::siblings::SiblingCache;
use super::{collected_type, date_hint, is_date_type, known_type, literal_union, CursorScope};
use crate::policy::blocks::{
    BlockKind, DecisionTableIr, ExpressionIr, IntelliSenseSource, MatchIr, ROW_ID_KEY,
};
use crate::policy::ir::PropertyTypeIr;
use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::db::{Db, Unit};
use crate::workspace::types::{BlockRef, Cursor, CursorTarget, ExpressionKind};

pub(super) fn policy_scope(
    db: &Db,
    cursor: &Cursor,
    cache: &mut SiblingCache,
) -> Option<CursorScope> {
    let block_ref = BlockRef {
        policy_path: cursor.policy_path.clone(),
        block_id: cursor.block_id.clone(),
    };
    let block = db.block_ir(&block_ref)?;
    let unit = db.unit(&cursor.policy_path);
    let enriched = db.enriched_of_unit(&unit);
    let scope = enriched.scope_before(&block_ref);
    match &block.kind {
        BlockKind::DecisionTable(table) => {
            table_scope(db, &unit, table, cursor, scope, &enriched.scope, cache)
        }
        BlockKind::Expression(expression) => expression_scope(db, &unit, expression, cursor, scope),
        BlockKind::Assertion(_) => assertion_scope(cursor, scope),
        BlockKind::Match(block) => match_scope(db, &unit, block, cursor, scope),
    }
}

fn table_scope(
    db: &Db,
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
                        // Calendar hints use the declared date format, while expression
                        // diagnostics/completions keep the actual string runtime type.
                        let field_type = return_type(db, field, &scope);
                        let hint = declared_type(db, unit, field).filter(is_date_type);
                        CursorScope::unary_with_hint(scope.with_dollar(&field_type), hint)
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
                    written_type(db, unit, written, column.field.as_ref())
                        .map(|t| collected_type(t, column.collect))
                })
                .or_else(|| {
                    cache.infer(cursor, || {
                        table
                            .rules
                            .iter()
                            .filter_map(|rule| {
                                let id = rule.get(ROW_ID_KEY)?.clone();
                                let t = rule
                                    .get(col)
                                    .filter(|cell| !cell.is_empty())
                                    .map(|cell| return_type(db, cell, &scope));
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

fn expression_scope(
    db: &Db,
    unit: &Unit,
    expression: &ExpressionIr,
    cursor: &Cursor,
    scope: VariableType,
) -> Option<CursorScope> {
    match &cursor.target {
        CursorTarget::ExpressionKey => Some(CursorScope::path(scope)),
        CursorTarget::Expression { .. } => {
            let expected = declared_type(db, unit, &expression.key);
            Some(CursorScope::value(scope, expected))
        }
        _ => None,
    }
}

fn assertion_scope(cursor: &Cursor, scope: VariableType) -> Option<CursorScope> {
    match &cursor.target {
        CursorTarget::AssertionOutput => Some(CursorScope::path(scope)),
        CursorTarget::Expression { .. } => Some(CursorScope::condition(scope)),
        _ => None,
    }
}

fn match_scope(
    db: &Db,
    unit: &Unit,
    block: &MatchIr,
    cursor: &Cursor,
    scope: VariableType,
) -> Option<CursorScope> {
    match &cursor.target {
        CursorTarget::MatchTarget => Some(CursorScope::path(scope)),
        CursorTarget::Expression { .. } => Some(CursorScope::condition(scope)),
        CursorTarget::MatchValue { .. } => {
            // Output arms define the inferred type; neighboring literals are not a
            // declaration constraining what a new arm may return.
            let expected = declared_type(db, unit, &block.key);
            Some(CursorScope::value(scope, expected))
        }
        _ => None,
    }
}

fn return_type(db: &Db, source: &Arc<str>, scope: &VariableType) -> VariableType {
    let intellisense = db.intellisense();
    let mut is = intellisense.borrow_mut();
    IntelliSenseSource::analyze(&mut is, source, ExpressionKind::Standard, scope)
        .return_type
        .shallow_clone()
}

fn declared_type(db: &Db, unit: &Unit, path: &str) -> Option<VariableType> {
    if path.is_empty() {
        return None;
    }
    let kind = known_type(db.snapshot().base_scope.resolve_at(path))?;
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
            date_hint(kind)
        } else {
            kind
        },
    )
}

fn written_type(db: &Db, unit: &Unit, scope: &VariableType, path: &str) -> Option<VariableType> {
    if path.is_empty() {
        return None;
    }
    declared_type(db, unit, path).or_else(|| literal_union(scope.resolve_at(path)))
}

use std::sync::Arc;

use zen_expression::variable::VariableType;

use super::{known_type, literal_union, CursorScope};
use crate::policy::blocks::{
    BlockKind, DecisionTableIr, ExpressionIr, IntelliSenseSource, MatchIr, ROW_ID_KEY,
};
use crate::policy::queries::scope::VariableTypeScope;
use crate::workspace::db::{Db, Unit};
use crate::workspace::types::{BlockRef, Cursor, CursorTarget, ExpressionKind};

pub(super) fn policy_scope(db: &Db, cursor: &Cursor) -> Option<CursorScope> {
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
            table_scope(db, &unit, table, cursor, scope, &enriched.scope)
        }
        BlockKind::Expression(expression) => expression_scope(db, expression, cursor, scope),
        BlockKind::Assertion(_) => assertion_scope(cursor, scope),
        BlockKind::Match(block) => match_scope(db, block, cursor, scope),
    }
}

fn table_scope(
    db: &Db,
    unit: &Unit,
    table: &DecisionTableIr,
    cursor: &Cursor,
    scope: VariableType,
    written: &VariableType,
) -> Option<CursorScope> {
    match &cursor.target {
        CursorTarget::DecisionTableHead { col } => {
            let known = table.inputs.iter().any(|c| c.id == *col)
                || table.outputs.iter().any(|c| c.id == *col);
            known.then(|| CursorScope::path(scope))
        }
        CursorTarget::DecisionTableCell { row, col } => {
            if let Some(column) = table.inputs.iter().find(|c| c.id == *col) {
                return Some(match column.field.as_ref().filter(|f| !f.is_empty()) {
                    Some(field) => {
                        let field_type = return_type(db, field, &scope);
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
                .or_else(|| written_type(db, written, column.field.as_ref()))
                .or_else(|| table_sibling_union(db, table, row, col, &scope));
            Some(CursorScope::value(scope, expected))
        }
        _ => None,
    }
}

fn expression_scope(
    db: &Db,
    expression: &ExpressionIr,
    cursor: &Cursor,
    scope: VariableType,
) -> Option<CursorScope> {
    match &cursor.target {
        CursorTarget::ExpressionKey => Some(CursorScope::path(scope)),
        CursorTarget::Expression { .. } => {
            let expected = declared_type(db, &expression.key);
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
    block: &MatchIr,
    cursor: &Cursor,
    scope: VariableType,
) -> Option<CursorScope> {
    match &cursor.target {
        CursorTarget::MatchTarget => Some(CursorScope::path(scope)),
        CursorTarget::Expression { .. } => Some(CursorScope::condition(scope)),
        CursorTarget::MatchValue { id } => {
            let expected =
                declared_type(db, &block.key).or_else(|| sibling_union(db, block, id, &scope));
            Some(CursorScope::value(scope, expected))
        }
        _ => None,
    }
}

fn table_sibling_union(
    db: &Db,
    table: &DecisionTableIr,
    row: &Arc<str>,
    col: &Arc<str>,
    scope: &VariableType,
) -> Option<VariableType> {
    let mut merged: Option<VariableType> = None;
    for rule in &table.rules {
        if rule.get(ROW_ID_KEY) == Some(row) {
            continue;
        }
        let Some(cell) = rule.get(col).filter(|c| !c.is_empty()) else {
            continue;
        };
        let cell_type = return_type(db, cell, scope);
        merged = Some(match merged {
            Some(acc) => acc.merge(&cell_type),
            None => cell_type,
        });
    }
    literal_union(merged?)
}

fn sibling_union(
    db: &Db,
    block: &MatchIr,
    arm_id: &Arc<str>,
    scope: &VariableType,
) -> Option<VariableType> {
    let mut merged: Option<VariableType> = None;
    for arm in &block.arms {
        if arm.id == *arm_id || arm.value.is_empty() {
            continue;
        }
        let arm_type = return_type(db, &arm.value, scope);
        merged = Some(match merged {
            Some(acc) => acc.merge(&arm_type),
            None => arm_type,
        });
    }
    literal_union(merged?)
}

fn return_type(db: &Db, source: &Arc<str>, scope: &VariableType) -> VariableType {
    let intellisense = db.intellisense();
    let mut is = intellisense.borrow_mut();
    IntelliSenseSource::analyze(&mut is, source, ExpressionKind::Standard, scope)
        .return_type
        .shallow_clone()
}

fn declared_type(db: &Db, path: &str) -> Option<VariableType> {
    if path.is_empty() {
        return None;
    }
    known_type(db.snapshot().base_scope.resolve_at(path))
}

fn written_type(db: &Db, scope: &VariableType, path: &str) -> Option<VariableType> {
    if path.is_empty() {
        return None;
    }
    declared_type(db, path).or_else(|| literal_union(scope.resolve_at(path)))
}

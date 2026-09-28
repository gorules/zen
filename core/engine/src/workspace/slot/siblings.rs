use std::sync::Arc;

use ahash::HashMap;
use zen_expression::variable::VariableType;

use super::CursorScope;
use crate::workspace::types::{Cursor, CursorTarget};

type ColumnKey = (Arc<str>, Arc<str>, Arc<str>);

#[derive(Default)]
pub(super) struct SiblingCache {
    columns: HashMap<ColumnKey, ColumnTypes>,
}

struct ColumnTypes {
    total: Option<VariableType>,
    excluding: HashMap<Arc<str>, Option<VariableType>>,
}

fn merge(a: Option<VariableType>, b: Option<&VariableType>) -> Option<VariableType> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.merge(b)),
        (a @ Some(_), None) => a,
        (None, b) => b.map(VariableType::shallow_clone),
    }
}

impl SiblingCache {
    pub(super) fn infer(
        &mut self,
        cursor: &Cursor,
        analyze: impl FnOnce() -> Vec<(Arc<str>, Option<VariableType>)>,
    ) -> Option<VariableType> {
        let (row, col) = match &cursor.target {
            CursorTarget::DecisionTableCell { row, col } => (row, col.clone()),
            CursorTarget::MatchValue { id } => (id, Arc::from("match")),
            _ => return None,
        };
        let key = (cursor.policy_path.clone(), cursor.block_id.clone(), col);
        let column = self.columns.entry(key).or_insert_with(|| {
            let cells = analyze();
            let mut prefixes = Vec::with_capacity(cells.len() + 1);
            prefixes.push(None);
            for (_, value) in &cells {
                prefixes.push(merge(prefixes.last().cloned().flatten(), value.as_ref()));
            }
            let total = prefixes.last().cloned().flatten();
            let mut suffix = None;
            let mut excluding = HashMap::default();
            for (index, (id, value)) in cells.iter().enumerate().rev() {
                excluding.insert(id.clone(), merge(prefixes[index].clone(), suffix.as_ref()));
                suffix = merge(value.clone(), suffix.as_ref());
            }
            ColumnTypes { total, excluding }
        });
        column
            .excluding
            .get(row)
            .unwrap_or(&column.total)
            .clone()
            .and_then(CursorScope::literal_union)
    }
}

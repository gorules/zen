use std::sync::Arc;

use ahash::HashMap;
use zen_expression::variable::VariableType;

use super::literal_union;
use crate::workspace::types::{Cursor, CursorTarget};

#[derive(Default)]
pub(super) struct SiblingCache {
    columns: HashMap<(Arc<str>, Arc<str>, Arc<str>), ColumnTypes>,
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
    /// The cache belongs to one query, so source/scope changes cannot reuse stale types.
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
            .and_then(literal_union)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_is_analyzed_once_and_excludes_only_the_active_row() {
        let mut cache = SiblingCache::default();
        let mut calls = 0;
        for (row, expected) in [
            ("a", vec!["b", "c"]),
            ("b", vec!["a", "c"]),
            ("c", vec!["a", "b"]),
            ("new", vec!["a", "b", "c"]),
        ] {
            let cursor = Cursor {
                policy_path: "p".into(),
                block_id: "t".into(),
                pos: 0,
                target: CursorTarget::DecisionTableCell {
                    row: row.into(),
                    col: "out".into(),
                },
            };
            let inferred = cache
                .infer(&cursor, || {
                    calls += 1;
                    ["a", "b", "c"]
                        .into_iter()
                        .map(|s| (Arc::from(s), Some(VariableType::Const(s.into()))))
                        .collect()
                })
                .unwrap();
            let VariableType::Enum(_, values) = inferred else {
                panic!("expected enum");
            };
            assert_eq!(
                values.iter().map(|v| v.as_ref()).collect::<Vec<_>>(),
                expected
            );
        }
        assert_eq!(calls, 1);
    }
}

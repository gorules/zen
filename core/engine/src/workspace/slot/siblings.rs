use std::sync::Arc;

use ahash::HashMap;
use zen_expression::variable::VariableType;

use super::CursorScope;
use crate::workspace::types::{Cursor, CursorTarget};

type ColumnKey = (Arc<str>, Arc<str>, Arc<str>);

#[derive(Default)]
pub(super) struct SiblingCache {
    columns: HashMap<ColumnKey, Option<ColumnTypes>>,
}

struct ColumnTypes {
    distinct: Vec<(VariableType, usize)>,
    rows: HashMap<Arc<str>, usize>,
}

impl ColumnTypes {
    const MAX_DISTINCT: usize = 64;

    fn collect(cells: impl IntoIterator<Item = (Arc<str>, Option<VariableType>)>) -> Option<Self> {
        let mut distinct: Vec<(VariableType, usize)> = Vec::new();
        let mut index_of: HashMap<String, usize> = HashMap::default();
        let mut rows = HashMap::default();
        for (id, value) in cells {
            let Some(value) = value else {
                continue;
            };
            let index = *index_of.entry(value.to_string()).or_insert_with(|| {
                distinct.push((value.shallow_clone(), 0));
                distinct.len() - 1
            });
            if distinct.len() > Self::MAX_DISTINCT {
                return None;
            }
            distinct[index].1 += 1;
            rows.insert(id, index);
        }
        Some(Self { distinct, rows })
    }

    fn union_without(&self, skip: Option<usize>) -> Option<VariableType> {
        self.distinct
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != skip)
            .map(|(_, (value, _))| value)
            .fold(None, |acc: Option<VariableType>, value| {
                Some(match acc {
                    Some(acc) => acc.merge(value),
                    None => value.shallow_clone(),
                })
            })
    }

    fn excluding(&self, row: &Arc<str>) -> Option<VariableType> {
        let own = self
            .rows
            .get(row)
            .copied()
            .filter(|index| self.distinct[*index].1 == 1);
        self.union_without(own)
    }
}

impl SiblingCache {
    pub(super) fn infer<I>(
        &mut self,
        cursor: &Cursor,
        analyze: impl FnOnce() -> I,
    ) -> Option<VariableType>
    where
        I: IntoIterator<Item = (Arc<str>, Option<VariableType>)>,
    {
        let (row, col) = match &cursor.target {
            CursorTarget::DecisionTableCell { row, col } => (row, col.clone()),
            CursorTarget::MatchValue { id } => (id, Arc::from("match")),
            _ => return None,
        };
        let key = (cursor.policy_path.clone(), cursor.block_id.clone(), col);
        self.columns
            .entry(key)
            .or_insert_with(|| ColumnTypes::collect(analyze()))
            .as_ref()?
            .excluding(row)
            .and_then(CursorScope::literal_union)
    }
}

use crate::nodes::decision_table::index::TableIndex;
use fixedbitset::FixedBitSet;
use std::sync::Arc;
use zen_types::decision::DecisionTableInputField;
use zen_types::variable::Variable;

pub(crate) struct Candidates;

impl Candidates {
    pub fn narrow<E>(
        index: &TableIndex,
        inputs: &[DecisionTableInputField],
        mut value: impl FnMut(usize, &Arc<str>) -> Result<Option<Variable>, E>,
    ) -> Result<Option<FixedBitSet>, E> {
        let mut acc: Option<FixedBitSet> = None;
        for (col_idx, column) in index.columns.iter().enumerate() {
            let Some(column) = column else {
                continue;
            };
            let Some(field) = inputs[col_idx].field.as_ref().filter(|f| !f.is_empty()) else {
                continue;
            };
            let Some(value) = value(col_idx, field)? else {
                return Ok(None);
            };
            if matches!(value, Variable::Dynamic(_)) {
                return Ok(None);
            }
            let hit = column.rows_for(&value);
            match &mut acc {
                None => {
                    let mut first = column.fallback.clone();
                    if let Some(hit) = hit {
                        first.union_with(hit);
                    }
                    acc = Some(first);
                }
                Some(acc) => {
                    let fallback = column.fallback.as_slice();
                    let hit = hit.map(FixedBitSet::as_slice).unwrap_or_default();
                    for (i, word) in acc.as_mut_slice().iter_mut().enumerate() {
                        let f = fallback.get(i).copied().unwrap_or(0);
                        let h = hit.get(i).copied().unwrap_or(0);
                        *word &= f | h;
                    }
                }
            }
        }
        Ok(acc)
    }
}

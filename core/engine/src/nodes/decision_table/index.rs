use ahash::HashMap;
use fixedbitset::FixedBitSet;
use rust_decimal::Decimal;
use std::sync::Arc;
use zen_expression::intellisense::{ArmTest, IntelliSense};
use zen_types::decision::DecisionTableInputField;
use zen_types::variable::Variable;

pub(crate) const MIN_INDEX_ROWS: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableIndex {
    pub(crate) columns: Vec<Option<ColumnIndex>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ColumnIndex {
    strings: HashMap<Arc<str>, FixedBitSet>,
    numbers: HashMap<Decimal, FixedBitSet>,
    bools: HashMap<bool, FixedBitSet>,
    captured: FixedBitSet,
    pub(crate) fallback: FixedBitSet,
}

impl TableIndex {
    pub(crate) fn build(
        inputs: &[DecisionTableInputField],
        rules: &[HashMap<Arc<str>, Arc<str>>],
    ) -> Option<TableIndex> {
        let rows = rules.len();
        if rows < MIN_INDEX_ROWS {
            return None;
        }
        let mut intellisense = IntelliSense::new();
        let columns: Vec<Option<ColumnIndex>> = inputs
            .iter()
            .map(|col| ColumnIndex::build(col, rules, rows, &mut intellisense))
            .collect();
        columns
            .iter()
            .any(Option::is_some)
            .then_some(TableIndex { columns })
    }

    pub(crate) fn decides(&self, col_idx: usize, row_idx: usize) -> bool {
        self.columns
            .get(col_idx)
            .and_then(Option::as_ref)
            .is_some_and(|c| c.captured.contains(row_idx))
    }
}

impl ColumnIndex {
    fn build(
        col: &DecisionTableInputField,
        rules: &[HashMap<Arc<str>, Arc<str>>],
        rows: usize,
        intellisense: &mut IntelliSense,
    ) -> Option<ColumnIndex> {
        if col.field.as_deref().is_none_or(|f| f.is_empty()) {
            return None;
        }
        let mut strings: HashMap<Arc<str>, FixedBitSet> = HashMap::default();
        let mut numbers: HashMap<Decimal, FixedBitSet> = HashMap::default();
        let mut bools: HashMap<bool, FixedBitSet> = HashMap::default();
        let mut captured = FixedBitSet::with_capacity(rows);
        let mut fallback = FixedBitSet::with_capacity(rows);

        for (row_idx, rule) in rules.iter().enumerate() {
            let Some(cell) = rule.get(&col.id).filter(|c| !c.is_empty()) else {
                fallback.insert(row_idx);
                continue;
            };
            match intellisense.cell_test(cell) {
                ArmTest::Enum { values, .. } => {
                    for value in values {
                        strings
                            .entry(Arc::from(value.as_ref()))
                            .or_insert_with(|| FixedBitSet::with_capacity(rows))
                            .insert(row_idx);
                    }
                    captured.insert(row_idx);
                }
                ArmTest::Bool { values, .. } => {
                    for value in values {
                        bools
                            .entry(value)
                            .or_insert_with(|| FixedBitSet::with_capacity(rows))
                            .insert(row_idx);
                    }
                    captured.insert(row_idx);
                }
                ArmTest::Number { cover, .. } => match cover.points() {
                    Some(points) => {
                        for point in points {
                            numbers
                                .entry(point.normalize())
                                .or_insert_with(|| FixedBitSet::with_capacity(rows))
                                .insert(row_idx);
                        }
                        captured.insert(row_idx);
                    }
                    None => {
                        fallback.insert(row_idx);
                    }
                },
                ArmTest::Default | ArmTest::Unrecognized => {
                    fallback.insert(row_idx);
                }
            }
        }

        (captured.count_ones(..) > 0).then_some(ColumnIndex {
            strings,
            numbers,
            bools,
            captured,
            fallback,
        })
    }

    pub(crate) fn rows_for(&self, value: &Variable) -> Option<&FixedBitSet> {
        match value {
            Variable::String(s) => self.strings.get(s.as_str()),
            Variable::Number(n) => self.numbers.get(&n.normalize()),
            Variable::Bool(b) => self.bools.get(b),
            _ => None,
        }
    }
}

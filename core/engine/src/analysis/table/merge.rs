use std::sync::Arc;

use ahash::HashMap;

use super::cell::CellConstraint;
use super::print::CellText;
use super::value_set::ValueSet;
use super::verify::{Finding, HitMode, VerifyTable};

const COMPRESS_BUDGET: usize = 4_000_000;
const MAX_ROUNDS: usize = 8;

#[derive(Clone)]
struct Row {
    rule: HashMap<Arc<str>, Arc<str>>,
    cells: Vec<CellConstraint>,
    fixed: bool,
}

struct Work {
    remaining: usize,
}

impl Work {
    fn spend(&mut self, amount: usize) -> bool {
        match self.remaining.checked_sub(amount.max(1)) {
            Some(left) => {
                self.remaining = left;
                true
            }
            None => {
                self.remaining = 0;
                false
            }
        }
    }
}

impl VerifyTable<'_> {
    pub(super) fn compress(
        &self,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
    ) -> Option<Finding> {
        if self.rules.len() < 2 {
            return None;
        }
        let mut rows: Vec<Option<Row>> = self
            .rules
            .iter()
            .zip(cells)
            .zip(satisfiable)
            .map(|((rule, cells), satisfiable)| {
                Some(Row {
                    rule: rule.clone(),
                    cells: cells.clone(),
                    fixed: !satisfiable,
                })
            })
            .collect();
        let before = rows.len();
        let mut work = Work {
            remaining: super::FullCheck::scale(COMPRESS_BUDGET),
        };
        for _ in 0..MAX_ROUNDS {
            let mut changed = false;
            for col in 0..self.inputs.len() {
                changed |= self.merge_column(&mut rows, col, &mut work);
            }
            if self.mode != HitMode::Collect {
                changed |= self.absorb(&mut rows, &mut work);
            }
            if !changed || work.remaining == 0 {
                break;
            }
        }
        let rules: Vec<_> = rows.into_iter().flatten().map(|row| row.rule).collect();
        (rules.len() < before).then_some(Finding::CompressibleTable { before, rules })
    }

    fn output_key(&self, row: &Row) -> Vec<String> {
        self.outputs
            .iter()
            .map(|col| {
                row.rule
                    .get(&col.id)
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn collects(&self, row: &Row) -> bool {
        self.outputs
            .iter()
            .any(|col| col.collect && row.rule.get(&col.id).is_some_and(|c| !c.trim().is_empty()))
    }

    fn cell_key(cell: &CellConstraint) -> CellConstraint {
        match cell {
            CellConstraint::Known(set) if set.is_all() => CellConstraint::Any,
            other => other.clone(),
        }
    }

    fn region(row: &Row) -> Vec<ValueSet> {
        row.cells
            .iter()
            .map(|cell| cell.known_set().unwrap_or_else(ValueSet::all))
            .collect()
    }

    fn catches(row: &Row, region: &[ValueSet]) -> bool {
        Self::region(row)
            .iter()
            .zip(region)
            .all(|(cell, wanted)| cell.intersects(wanted))
    }

    fn within(inner: &Row, outer: &Row) -> bool {
        inner
            .cells
            .iter()
            .zip(&outer.cells)
            .all(|(a, b)| match (a, b) {
                (_, CellConstraint::Any) => true,
                (CellConstraint::Opaque(x), CellConstraint::Opaque(y)) => x == y,
                (CellConstraint::Opaque(_), _) | (_, CellConstraint::Opaque(_)) => false,
                (a, b) => match (a.known_set(), b.known_set()) {
                    (Some(a), Some(b)) => a.is_subset(&b),
                    _ => false,
                },
            })
    }

    fn clear_between(
        rows: &[Option<Row>],
        from: usize,
        to: usize,
        region: &[ValueSet],
        work: &mut Work,
    ) -> bool {
        if !work.spend(to.saturating_sub(from)) {
            return false;
        }
        rows[from + 1..to]
            .iter()
            .flatten()
            .all(|between| !Self::catches(between, region))
    }

    fn absorb(&self, rows: &mut [Option<Row>], work: &mut Work) -> bool {
        let mut buckets: HashMap<Vec<String>, Vec<usize>> = HashMap::default();
        for (idx, row) in rows.iter().enumerate() {
            if let Some(row) = row.as_ref().filter(|r| !r.fixed && !self.collects(r)) {
                buckets.entry(self.output_key(row)).or_default().push(idx);
            }
        }
        let mut changed = false;
        let mut groups: Vec<Vec<usize>> = buckets.into_values().filter(|g| g.len() > 1).collect();
        groups.sort_unstable_by_key(|g| g[0]);
        for group in groups {
            for &inner in &group {
                let Some(inner_row) = rows[inner].as_ref() else {
                    continue;
                };
                let region = Self::region(inner_row);
                let mut absorbed = false;
                for &outer in &group {
                    if outer == inner {
                        continue;
                    }
                    if !work.spend(inner_row.cells.len()) {
                        return changed;
                    }
                    let Some(outer_row) = rows[outer].as_ref() else {
                        continue;
                    };
                    if !Self::within(inner_row, outer_row) {
                        continue;
                    }
                    if outer < inner || Self::clear_between(rows, inner, outer, &region, work) {
                        absorbed = true;
                        break;
                    }
                }
                if absorbed {
                    rows[inner] = None;
                    changed = true;
                }
            }
        }
        changed
    }

    fn merge_column(&self, rows: &mut [Option<Row>], col: usize, work: &mut Work) -> bool {
        let mut buckets: HashMap<(Vec<String>, Vec<CellConstraint>), Vec<usize>> =
            HashMap::default();
        for (idx, row) in rows.iter().enumerate() {
            let Some(row) = row.as_ref().filter(|r| !r.fixed) else {
                continue;
            };
            if row.cells[col].known_set().is_none() {
                continue;
            }
            if !work.spend(row.cells.len()) {
                return false;
            }
            let others: Vec<CellConstraint> = row
                .cells
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != col)
                .map(|(_, cell)| Self::cell_key(cell))
                .collect();
            buckets
                .entry((self.output_key(row), others))
                .or_default()
                .push(idx);
        }
        let mut changed = false;
        let mut groups: Vec<Vec<usize>> = buckets.into_values().filter(|g| g.len() > 1).collect();
        groups.sort_unstable_by_key(|g| g[0]);
        for group in groups {
            let mut keep = group[0];
            for &next in &group[1..] {
                if self.merge_into(rows, keep, next, col, work) {
                    changed = true;
                } else {
                    keep = next;
                }
                if work.remaining == 0 {
                    return changed;
                }
            }
        }
        changed
    }

    fn merge_into(
        &self,
        rows: &mut [Option<Row>],
        keep: usize,
        next: usize,
        col: usize,
        work: &mut Work,
    ) -> bool {
        let (Some(keep_row), Some(next_row)) = (rows[keep].as_ref(), rows[next].as_ref()) else {
            return false;
        };
        let (Some(a), Some(b)) = (
            keep_row.cells[col].known_set(),
            next_row.cells[col].known_set(),
        ) else {
            return false;
        };
        if (self.mode == HitMode::Collect || self.collects(keep_row)) && a.intersects(&b) {
            return false;
        }
        let moved: Vec<ValueSet> = Self::region(next_row)
            .into_iter()
            .enumerate()
            .map(|(idx, set)| if idx == col { set.difference(&a) } else { set })
            .collect();
        if self.mode != HitMode::Collect && !Self::clear_between(rows, keep, next, &moved, work) {
            return false;
        }
        let union = a.union(&b);
        let Some(text) = CellText::of(&union, &ValueSet::all(), self.inputs[col].dated) else {
            return false;
        };
        let id = self.inputs[col].id.clone();
        if let Some(row) = rows[keep].as_mut() {
            row.rule.insert(id, Arc::from(text.as_str()));
            row.cells[col] = if text.is_empty() {
                CellConstraint::Any
            } else {
                CellConstraint::Known(union)
            };
        }
        rows[next] = None;
        true
    }

    pub(super) fn covering_cells(
        &self,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
    ) -> Vec<Finding> {
        let mut findings = Vec::new();
        for (row, row_cells) in cells.iter().enumerate() {
            if !satisfiable[row] {
                continue;
            }
            for (col, cell) in self.inputs.iter().zip(row_cells) {
                let (CellConstraint::Known(set), Some(domain)) = (cell, &col.domain) else {
                    continue;
                };
                if !domain.is_empty() && domain.difference(set).is_empty() {
                    findings.push(Finding::CellCoversDomain {
                        row,
                        col: col.id.clone(),
                    });
                }
            }
        }
        findings
    }
}

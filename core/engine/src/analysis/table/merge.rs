use std::borrow::Cow;
use std::sync::Arc;

use ahash::HashMap;

use super::cell::CellConstraint;
use super::index::RowIndex;
use super::intern::Interner;
use super::print::CellText;
use super::value_set::{StringSet, Strings, ValueSet};
use super::verify::{Finding, HitMode, VerifyTable};

const COMPRESS_BUDGET: usize = 50_000_000;
const MAX_ROUNDS: usize = 8;

struct Row<'a> {
    rule: Cow<'a, HashMap<Arc<str>, Arc<str>>>,
    cells: Cow<'a, [CellConstraint]>,
    output: u32,
    fixed: bool,
    barrier: bool,
    stale: Vec<usize>,
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

pub(super) struct CellKeys<'c> {
    columns: usize,
    given: HashMap<&'c CellConstraint, u32>,
    made: HashMap<CellConstraint, u32>,
    of: Vec<u32>,
    pending: Vec<usize>,
}

impl<'c> CellKeys<'c> {
    const ANY: u32 = 0;
    const PENDING: u32 = u32::MAX;

    pub(super) fn new(cells: &'c [Vec<CellConstraint>], columns: usize) -> Self {
        let mut given: HashMap<&'c CellConstraint, u32> = HashMap::default();
        let mut of = Vec::with_capacity(cells.len() * columns);
        for row in cells {
            for cell in row {
                of.push(match cell {
                    CellConstraint::Any => Self::ANY,
                    CellConstraint::Known(set) if set.is_all() => Self::ANY,
                    other => {
                        let next = given.len() as u32 + 1;
                        *given.entry(other).or_insert(next)
                    }
                });
            }
        }
        Self {
            columns,
            given,
            made: HashMap::default(),
            of,
            pending: Vec::new(),
        }
    }

    fn id(&mut self, cell: &CellConstraint) -> u32 {
        match cell {
            CellConstraint::Any => Self::ANY,
            CellConstraint::Known(set) if set.is_all() => Self::ANY,
            other => match self.given.get(other).or_else(|| self.made.get(other)) {
                Some(&id) => id,
                None => {
                    let id = (self.given.len() + self.made.len()) as u32 + 1;
                    self.made.insert(other.clone(), id);
                    id
                }
            },
        }
    }

    pub(super) fn row(&self, row: usize) -> &[u32] {
        &self.of[row * self.columns..(row + 1) * self.columns]
    }

    fn touch(&mut self, row: usize, col: usize) {
        let slot = row * self.columns + col;
        if self.of[slot] != Self::PENDING {
            self.of[slot] = Self::PENDING;
            self.pending.push(slot);
        }
    }

    fn settle(&mut self, rows: &[Option<Row>]) {
        for slot in std::mem::take(&mut self.pending) {
            let (row, col) = (slot / self.columns, slot % self.columns);
            self.of[slot] = match &rows[row] {
                Some(row) => self.id(&row.cells[col]),
                None => Self::ANY,
            };
        }
    }
}

impl VerifyTable<'_> {
    pub(super) fn compress(
        &self,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
        warned: &[bool],
        keys: CellKeys<'_>,
        outputs: &[u32],
    ) -> Option<Finding> {
        let found = self.compressed(cells, satisfiable, warned, keys, outputs);
        #[cfg(test)]
        assert_eq!(
            found,
            super::merge_reference::Legacy(self).compress(cells, satisfiable, warned),
            "table compression diverged from the reference implementation"
        );
        found
    }

    fn compressed(
        &self,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
        warned: &[bool],
        mut keys: CellKeys,
        outputs: &[u32],
    ) -> Option<Finding> {
        if self.rules.len() < 2 {
            return None;
        }
        let all = ValueSet::all();
        let mut rows: Vec<Option<Row>> = self
            .rules
            .iter()
            .zip(cells)
            .zip(satisfiable)
            .zip(warned)
            .zip(outputs)
            .map(|((((rule, cells), satisfiable), warned), &output)| {
                Some(Row {
                    rule: Cow::Borrowed(rule),
                    cells: Cow::Borrowed(cells.as_slice()),
                    output,
                    fixed: !satisfiable || *warned,
                    barrier: *warned,
                    stale: Vec::new(),
                })
            })
            .collect();
        let before = rows.len();
        let mut work = Work {
            remaining: COMPRESS_BUDGET,
        };
        let mut index = RowIndex::blocking(cells, self.inputs.len());
        for _ in 0..MAX_ROUNDS {
            let mut changed = false;
            if self.mode != HitMode::Collect {
                changed |= self.drop_shadowed(&mut rows, &mut work, &mut index, &all);
                changed |= self.absorb(&mut rows, &mut work, &mut index, &all);
            }
            for col in 0..self.inputs.len() {
                changed |=
                    self.merge_column(&mut rows, col, &mut work, &mut index, &mut keys, &all);
            }
            if !changed || work.remaining == 0 {
                break;
            }
        }
        if rows.iter().flatten().count() >= before {
            return None;
        }
        let rules: Vec<_> = rows
            .into_iter()
            .flatten()
            .map(|mut row| {
                for &col in &row.stale {
                    if let CellConstraint::Known(set) = &row.cells[col] {
                        if let Some(text) = self.cell_text(col, set, &all, false) {
                            row.rule
                                .to_mut()
                                .insert(self.inputs[col].id.clone(), Arc::from(text.as_str()));
                        }
                    }
                }
                row.rule.into_owned()
            })
            .collect();
        Some(Finding::CompressibleTable { before, rules })
    }

    fn collects(&self, row: &Row) -> bool {
        self.outputs
            .iter()
            .any(|col| col.collect && row.rule.get(&col.id).is_some_and(|c| !c.trim().is_empty()))
    }

    fn region<'r>(row: &'r Row, all: &'r ValueSet) -> Vec<&'r ValueSet> {
        row.cells
            .iter()
            .map(|cell| match cell {
                CellConstraint::Known(set) => set,
                _ => all,
            })
            .collect()
    }

    fn catches(row: &Row, region: &[&ValueSet]) -> bool {
        row.cells
            .iter()
            .zip(region)
            .all(|(cell, wanted)| match cell {
                CellConstraint::Known(set) => set.intersects(wanted),
                _ => !wanted.is_empty(),
            })
    }

    fn within(inner: &Row, outer: &Row) -> bool {
        inner
            .cells
            .iter()
            .zip(outer.cells.iter())
            .all(|(a, b)| match (a, b) {
                (_, CellConstraint::Any) => true,
                (CellConstraint::Opaque(x), CellConstraint::Opaque(y)) => x == y,
                (CellConstraint::Opaque(_), _) | (_, CellConstraint::Opaque(_)) => false,
                (CellConstraint::Known(a), CellConstraint::Known(b)) => a.is_subset(b),
                (CellConstraint::Any, CellConstraint::Known(b)) => b.is_all(),
            })
    }

    fn clear_between(
        rows: &[Option<Row>],
        from: usize,
        to: usize,
        region: &[&ValueSet],
        work: &mut Work,
        index: &mut RowIndex,
    ) -> bool {
        let (low, high) = (from.min(to), from.max(to));
        if !work.spend(high - low)
            || rows[low + 1..high]
                .iter()
                .flatten()
                .any(|between| between.barrier)
        {
            return false;
        }
        let mut visited = 0usize;
        let mut clear = true;
        let indexed = index.overlapping(region, to.saturating_sub(from), |e| {
            visited += 1;
            if clear && from < e && e < to {
                if let Some(between) = &rows[e] {
                    clear = !Self::catches(between, region);
                }
            }
        });
        if indexed {
            return work.spend(visited) && clear;
        }
        if !work.spend(to.saturating_sub(from)) {
            return false;
        }
        rows[from + 1..to]
            .iter()
            .flatten()
            .all(|between| !Self::catches(between, region))
    }

    fn fills(row: &Row, id: &Arc<str>) -> bool {
        row.rule.get(id).is_some_and(|c| !c.trim().is_empty())
    }

    fn drop_shadowed(
        &self,
        rows: &mut [Option<Row>],
        work: &mut Work,
        index: &mut RowIndex,
        all: &ValueSet,
    ) -> bool {
        if self.mode != HitMode::RowFirst {
            return false;
        }
        let mut changed = false;
        for inner in 1..rows.len() {
            let shadowed = {
                let Some(inner_row) = rows[inner].as_ref() else {
                    continue;
                };
                if inner_row.fixed
                    || self
                        .outputs
                        .iter()
                        .any(|col| Self::fills(inner_row, &col.id))
                {
                    continue;
                }
                let region = Self::region(inner_row, all);
                let mut earlier: Vec<usize> = Vec::new();
                let indexed = index.overlapping(&region, inner, |e| {
                    if e < inner {
                        earlier.push(e);
                    }
                });
                if !indexed {
                    earlier = (0..inner).collect();
                }
                if !work.spend(earlier.len()) {
                    return changed;
                }
                earlier.iter().any(|&e| {
                    rows[e]
                        .as_ref()
                        .is_some_and(|outer| !outer.fixed && Self::within(inner_row, outer))
                })
            };
            if shadowed {
                rows[inner] = None;
                changed = true;
            }
        }
        changed
    }

    fn absorb(
        &self,
        rows: &mut [Option<Row>],
        work: &mut Work,
        index: &mut RowIndex,
        all: &ValueSet,
    ) -> bool {
        let mut buckets: HashMap<u32, Vec<usize>> = HashMap::default();
        for (idx, row) in rows.iter().enumerate() {
            if let Some(row) = row.as_ref().filter(|r| !r.fixed && !self.collects(r)) {
                buckets.entry(row.output).or_default().push(idx);
            }
        }
        let mut changed = false;
        let mut groups: Vec<Vec<usize>> = buckets.into_values().filter(|g| g.len() > 1).collect();
        groups.sort_unstable_by_key(|g| g[0]);
        let mut group_of: Vec<usize> = vec![usize::MAX; rows.len()];
        for (id, group) in groups.iter().enumerate() {
            for &row in group {
                group_of[row] = id;
            }
        }
        for (id, group) in groups.iter().enumerate() {
            for &inner in group {
                let absorbed = {
                    let Some(inner_row) = rows[inner].as_ref() else {
                        continue;
                    };
                    let region = Self::region(inner_row, all);
                    let mut outers: Vec<usize> = Vec::new();
                    let indexed = index.overlapping(&region, group.len(), |e| {
                        if group_of[e] == id {
                            outers.push(e);
                        }
                    });
                    if indexed {
                        outers.sort_unstable();
                    } else {
                        outers = group.clone();
                    }
                    let mut absorbed = false;
                    for outer in outers {
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
                        if outer < inner
                            || Self::clear_between(rows, inner, outer, &region, work, index)
                        {
                            absorbed = true;
                            break;
                        }
                    }
                    absorbed
                };
                if absorbed {
                    rows[inner] = None;
                    changed = true;
                }
            }
        }
        changed
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_column(
        &self,
        rows: &mut [Option<Row>],
        col: usize,
        work: &mut Work,
        index: &mut RowIndex,
        keys: &mut CellKeys,
        all: &ValueSet,
    ) -> bool {
        let columns = self.inputs.len();
        keys.settle(rows);
        let mut buckets: Interner<u32> = Interner::new(columns);
        let mut key: Vec<u32> = Vec::with_capacity(columns);
        let mut members: Vec<(u32, usize)> = Vec::new();
        for (idx, row) in rows.iter().enumerate() {
            let Some(row) = row.as_ref().filter(|r| !r.fixed) else {
                continue;
            };
            if matches!(row.cells[col], CellConstraint::Opaque(_)) {
                continue;
            }
            if !work.spend(row.cells.len()) {
                return false;
            }
            key.clear();
            key.push(row.output);
            key.extend(
                keys.row(idx)
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != col)
                    .map(|(_, &id)| id),
            );
            members.push((buckets.intern(&key), idx));
        }
        let mut starts = vec![0usize; buckets.len() + 1];
        for &(bucket, _) in &members {
            starts[bucket as usize + 1] += 1;
        }
        for bucket in 0..buckets.len() {
            starts[bucket + 1] += starts[bucket];
        }
        let mut cursor = starts.clone();
        let mut grouped = vec![0usize; members.len()];
        for &(bucket, idx) in &members {
            grouped[cursor[bucket as usize]] = idx;
            cursor[bucket as usize] += 1;
        }
        let mut changed = false;
        for bucket in 0..buckets.len() {
            let group = &grouped[starts[bucket]..starts[bucket + 1]];
            if group.len() < 2 {
                continue;
            }
            let mut keep = group[0];
            for &next in &group[1..] {
                if self.merge_into(rows, keep, next, col, work, index, keys, all) {
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

    #[allow(clippy::too_many_arguments)]
    fn merge_into(
        &self,
        rows: &mut [Option<Row>],
        keep: usize,
        next: usize,
        col: usize,
        work: &mut Work,
        index: &mut RowIndex,
        keys: &mut CellKeys,
        all: &ValueSet,
    ) -> bool {
        let (Some(keep_row), Some(next_row)) = (rows[keep].as_ref(), rows[next].as_ref()) else {
            return false;
        };
        if let (false, Some(a), Some(b)) = (
            self.inputs[col].dated,
            Self::plain(&keep_row.cells[col]),
            Self::plain(&next_row.cells[col]),
        ) {
            if (self.mode == HitMode::Collect || self.collects(keep_row))
                && b.iter().any(|key| a.contains(key))
            {
                return false;
            }
            let fresh: Strings = b.iter().filter(|key| !a.contains(key)).cloned().collect();
            if fresh.iter().any(|key| CellText::string(key).is_none()) {
                return false;
            }
            let added = ValueSet {
                strings: StringSet::Finite(fresh.clone()),
                ..ValueSet::empty()
            };
            let mut moved = Self::region(next_row, all);
            moved[col] = &added;
            if !Self::clear_between(rows, keep, next, &moved, work, index) {
                return false;
            }
            if let Some(row) = rows[keep].as_mut() {
                if let CellConstraint::Known(ValueSet {
                    strings: StringSet::Finite(members),
                    ..
                }) = &mut row.cells.to_mut()[col]
                {
                    members.extend(fresh.iter().cloned());
                }
                if !row.stale.contains(&col) {
                    row.stale.push(col);
                }
                keys.touch(keep, col);
                index.insert_strings(col, &fresh, keep);
            }
            rows[next] = None;
            return true;
        }
        let (Some(a), Some(b)) = (
            keep_row.cells[col].known_set(),
            next_row.cells[col].known_set(),
        ) else {
            return false;
        };
        if (self.mode == HitMode::Collect || self.collects(keep_row)) && a.intersects(&b) {
            return false;
        }
        let outside = b.difference(&a);
        let mut moved = Self::region(next_row, all);
        moved[col] = &outside;
        if !Self::clear_between(rows, keep, next, &moved, work, index) {
            return false;
        }
        let union = a.union(&b);
        let Some(text) = self.cell_text(col, &union, all, self.inputs[col].dated) else {
            return false;
        };
        let id = self.inputs[col].id.clone();
        if let Some(row) = rows[keep].as_mut() {
            row.rule.to_mut().insert(id, Arc::from(text.as_str()));
            row.stale.retain(|&stale| stale != col);
            row.cells.to_mut()[col] = if text.is_empty() {
                CellConstraint::Any
            } else {
                CellConstraint::Known(union)
            };
            keys.touch(keep, col);
            index.insert(&row.cells, keep);
        }
        rows[next] = None;
        true
    }

    fn plain(cell: &CellConstraint) -> Option<&Strings> {
        match cell {
            CellConstraint::Known(set)
                if set.numbers.is_empty() && set.bools == 0 && !set.null && !set.other =>
            {
                match &set.strings {
                    StringSet::Finite(keys) if !keys.is_empty() => Some(keys),
                    _ => None,
                }
            }
            _ => None,
        }
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
                debug_assert_eq!(
                    domain.is_subset(set),
                    domain.difference(set).is_empty(),
                    "subset check disagrees with the difference for {set:?}"
                );
                if !domain.is_empty() && domain.is_subset(set) {
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

use std::rc::Rc;

use ahash::HashMap;

use super::cell::CellConstraint;
use super::gaps::{Bits, Dense, Space};
use super::partition::Partition;
use super::value_set::ValueSet;
use super::verify::{Budget, MAX_FRAGMENTS, MAX_MINIMIZE};

pub(super) struct Coverage {
    space: Space,
    columns: usize,
    bits: Vec<u64>,
    opaque: Vec<u32>,
    coverable: Vec<bool>,
    remaining: Vec<u64>,
    next: Vec<u64>,
    prefix: Vec<u64>,
}

impl Coverage {
    pub(super) fn new(cells: &[Vec<CellConstraint>], columns: usize) -> Option<Self> {
        let mut set_of: Vec<Vec<Option<u32>>> = Vec::with_capacity(columns);
        let mut sets_of: Vec<Vec<&ValueSet>> = Vec::with_capacity(columns);
        let everything = ValueSet::all();
        for column in 0..columns {
            let mut index: HashMap<&ValueSet, u32> = HashMap::default();
            let mut distinct: Vec<&ValueSet> = Vec::new();
            let ids: Vec<Option<u32>> = cells
                .iter()
                .map(|row| match &row[column] {
                    CellConstraint::Known(set) => Some(*index.entry(set).or_insert_with(|| {
                        distinct.push(set);
                        distinct.len() as u32 - 1
                    })),
                    _ => None,
                })
                .collect();
            set_of.push(ids);
            sets_of.push(distinct);
        }
        let atoms = sets_of
            .iter()
            .map(|sets| Dense::atoms(sets.iter().copied()));
        if !Dense::fits(cells.len(), atoms) {
            return None;
        }
        let mut partitions: Vec<Partition> = Vec::with_capacity(columns);
        let mut atoms_of: Vec<Vec<Vec<usize>>> = Vec::with_capacity(columns);
        for distinct in sets_of {
            let owned: Vec<ValueSet> = distinct.into_iter().cloned().collect();
            let partition = Partition::new(&everything, &owned);
            atoms_of.push(owned.iter().map(|set| partition.touching(set)).collect());
            partitions.push(partition);
        }
        let layouts: Vec<_> = partitions.iter().map(Partition::layout).collect();
        let widths: Vec<usize> = layouts.iter().map(|layout| layout.atoms).collect();
        let space = Space::new(layouts);
        let stride = space.stride();

        let mut opaque_ids: HashMap<Rc<str>, u32> = HashMap::default();
        let mut bits = vec![0u64; cells.len() * stride];
        let mut opaque = vec![0u32; cells.len() * columns];
        let mut coverable = vec![true; cells.len()];
        for (row, row_cells) in cells.iter().enumerate() {
            let words = &mut bits[row * stride..(row + 1) * stride];
            for (column, (axis, cell)) in space.axes().iter().zip(row_cells).enumerate() {
                let words = &mut words[axis.range()];
                match (cell, set_of[column][row]) {
                    (CellConstraint::Known(set), Some(id)) => {
                        coverable[row] &= !set.is_empty();
                        atoms_of[column][id as usize]
                            .iter()
                            .for_each(|&atom| Bits::set(words, atom));
                    }
                    (cell, _) => {
                        (0..widths[column]).for_each(|atom| Bits::set(words, atom));
                        if let CellConstraint::Opaque(atom) = cell {
                            let next = opaque_ids.len() as u32 + 1;
                            opaque[row * columns + column] =
                                *opaque_ids.entry(atom.clone()).or_insert(next);
                        }
                    }
                }
            }
        }
        Some(Self {
            space,
            columns,
            bits,
            opaque,
            coverable,
            remaining: Vec::new(),
            next: Vec::new(),
            prefix: Vec::new(),
        })
    }

    fn projects(&self, earlier: usize, row: usize) -> bool {
        let columns = self.columns;
        self.coverable[earlier]
            && self.opaque[earlier * columns..(earlier + 1) * columns]
                .iter()
                .zip(&self.opaque[row * columns..(row + 1) * columns])
                .all(|(&theirs, &own)| theirs == 0 || theirs == own)
    }

    pub(super) fn covered_by(
        &mut self,
        row: usize,
        requirements: &[Vec<usize>],
        budget: &mut Budget,
    ) -> Option<Option<Vec<usize>>> {
        let mut cited: Vec<usize> = Vec::new();
        for earlier in requirements {
            let coverers: Vec<usize> = earlier
                .iter()
                .copied()
                .filter(|&e| self.projects(e, row))
                .collect();
            let Some(used) = self.cover(row, &coverers, budget)? else {
                return Some(None);
            };
            cited.extend(self.minimize(row, used, budget)?);
        }
        cited.sort_unstable();
        cited.dedup();
        Some(Some(cited))
    }

    fn cover(
        &mut self,
        row: usize,
        coverers: &[usize],
        budget: &mut Budget,
    ) -> Option<Option<Vec<usize>>> {
        let Self {
            space,
            columns,
            bits,
            remaining,
            next,
            prefix,
            ..
        } = self;
        let stride = space.stride();
        remaining.clear();
        remaining.extend_from_slice(&bits[row * stride..(row + 1) * stride]);
        let mut used = Vec::new();
        for &earlier in coverers {
            let cut = &bits[earlier * stride..(earlier + 1) * stride];
            next.clear();
            let mut touched = false;
            for fragment in remaining.chunks_exact(stride) {
                budget.spend(*columns)?;
                match space.subtract(fragment, cut, prefix, next) {
                    true => touched = true,
                    false => next.extend_from_slice(fragment),
                }
            }
            if next.len() / stride > MAX_FRAGMENTS {
                return None;
            }
            if touched {
                used.push(earlier);
            }
            std::mem::swap(remaining, next);
            if remaining.is_empty() {
                return Some(Some(used));
            }
        }
        Some(None)
    }

    fn minimize(
        &mut self,
        row: usize,
        used: Vec<usize>,
        budget: &mut Budget,
    ) -> Option<Vec<usize>> {
        if used.len() > MAX_MINIMIZE {
            return Some(used);
        }
        let mut kept = used;
        let mut i = kept.len();
        while i > 0 {
            i -= 1;
            let candidate: Vec<usize> = kept
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, &earlier)| earlier)
                .collect();
            if self.cover(row, &candidate, budget)?.is_some() {
                kept.remove(i);
            }
        }
        Some(kept)
    }
}

use ahash::HashMap;

use super::cell::CellConstraint;
use super::missing::{Dimension, Pool};
use super::partition::Partition;
use super::value_set::ValueSet;
use super::verify::{Finding, Region, VerifyTable, MAX_FRAGMENTS};

const GAP_BUDGET: usize = 10_000_000;
const DIRECT_ROWS: usize = 8;

impl VerifyTable<'_> {
    pub(super) fn missing_sets(
        &self,
        dims: Vec<Dimension>,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
    ) -> Option<Option<Finding>> {
        let cuts: Vec<Region> = cells
            .iter()
            .zip(satisfiable)
            .filter(|(_, satisfiable)| **satisfiable)
            .map(|(row_cells, _)| {
                dims.iter()
                    .map(|d| {
                        d.columns
                            .iter()
                            .fold(ValueSet::all(), |acc, &col| match &row_cells[col] {
                                CellConstraint::Known(set) => acc.intersect(set),
                                _ => acc,
                            })
                    })
                    .collect()
            })
            .collect();
        let mut gaps = Gaps {
            cuts: &cuts,
            work: GAP_BUDGET,
            out: Vec::new(),
        };
        gaps.uncovered(
            dims.iter().map(|d| d.domain.clone()).collect(),
            (0..cuts.len()).collect(),
        )?;
        let remaining: Vec<Region> = gaps
            .out
            .into_iter()
            .filter_map(|mut fragment| {
                for (dim, set) in dims.iter().zip(fragment.iter_mut()) {
                    if dim.integer {
                        set.numbers = set.numbers.integral();
                    }
                }
                fragment
                    .iter()
                    .all(|set| !set.is_empty())
                    .then_some(fragment)
            })
            .collect();
        drop(cuts);
        if remaining.is_empty() {
            return Some(None);
        }
        let mut values: Vec<Vec<ValueSet>> = dims.iter().map(|_| Vec::new()).collect();
        let mut indexes: Vec<HashMap<(u64, u32), u32>> =
            dims.iter().map(|_| HashMap::default()).collect();
        let mut rows: Vec<u32> = Vec::with_capacity(remaining.len() * dims.len());
        for fragment in remaining {
            for ((set, sets), index) in fragment.into_iter().zip(&mut values).zip(&mut indexes) {
                rows.push(Pool::intern(sets, index, set));
            }
        }
        let atoms = dims.iter().map(|_| Vec::new()).collect();
        let pools = values
            .into_iter()
            .zip(indexes)
            .map(|(sets, index)| Pool::sets(sets, index))
            .collect();
        Some(Some(self.missing_cases_of(dims, atoms, pools, rows)))
    }
}

type Atoms = Vec<(ValueSet, Vec<usize>)>;

struct Split {
    dim: usize,
    score: usize,
    groups: Atoms,
    of_row: Vec<usize>,
    distinct: usize,
}

struct Gaps<'c> {
    cuts: &'c [Region],
    work: usize,
    out: Vec<Region>,
}

impl Gaps<'_> {
    fn spend(&mut self, amount: usize) -> Option<()> {
        self.work = self.work.checked_sub(amount.max(1))?;
        Some(())
    }

    fn uncovered(&mut self, region: Region, rows: Vec<usize>) -> Option<()> {
        self.spend(rows.len() * region.len())?;
        let rows: Vec<usize> = rows
            .into_iter()
            .filter(|&row| {
                self.cuts[row]
                    .iter()
                    .zip(&region)
                    .all(|(cut, set)| cut.intersects(set))
            })
            .collect();
        if rows.is_empty() {
            self.out.push(region);
            return Some(());
        }
        let covered = rows.iter().any(|&row| {
            region
                .iter()
                .zip(&self.cuts[row])
                .all(|(set, cut)| set.is_subset(cut))
        });
        if covered {
            return Some(());
        }
        if rows.len() > DIRECT_ROWS {
            if let Some((dim, atoms)) = self.split(&region, &rows) {
                for (atom, atom_rows) in atoms {
                    let mut piece = region.clone();
                    piece[dim] = atom;
                    self.uncovered(piece, atom_rows)?;
                }
                return Some(());
            }
        }
        self.subtract_all(region, &rows)
    }

    fn subtract_all(&mut self, region: Region, rows: &[usize]) -> Option<()> {
        let mut remaining = vec![region];
        let cuts = self.cuts;
        for &row in rows {
            let cut: Vec<&ValueSet> = cuts[row].iter().collect();
            let mut next = Vec::with_capacity(remaining.len());
            for fragment in remaining {
                self.spend(fragment.len())?;
                match VerifyTable::subtract(&fragment, &cut) {
                    Some(pieces) => next.extend(pieces),
                    None => next.push(fragment),
                }
            }
            if next.len() > MAX_FRAGMENTS {
                return None;
            }
            remaining = next;
            if remaining.is_empty() {
                return Some(());
            }
        }
        self.out.extend(remaining);
        Some(())
    }

    fn split(&mut self, region: &Region, rows: &[usize]) -> Option<(usize, Atoms)> {
        let mut best: Option<Split> = None;
        for (dim, bounds) in region.iter().enumerate() {
            let mut sets: Vec<ValueSet> = Vec::new();
            let mut index: HashMap<ValueSet, usize> = HashMap::default();
            let mut of_row: Vec<usize> = Vec::with_capacity(rows.len());
            for &row in rows {
                let set = self.cuts[row][dim].intersect(bounds);
                let next = sets.len();
                let slot = *index.entry(set.clone()).or_insert(next);
                if slot == next {
                    sets.push(set);
                }
                of_row.push(slot);
            }
            if sets.iter().all(|set| set == bounds) {
                continue;
            }
            let partition = Partition::new(bounds, &sets);
            if partition.touches(&sets) > self.work {
                return None;
            }
            let groups = partition.groups(&sets);
            if groups.len() < 2 {
                continue;
            }
            let mut count = vec![0usize; sets.len()];
            for &slot in &of_row {
                count[slot] += 1;
            }
            let score: usize = groups
                .iter()
                .map(|(_, ids)| ids.iter().map(|&id| count[id]).sum::<usize>())
                .sum();
            self.spend(score + groups.len())?;
            if best.as_ref().is_none_or(|split| score < split.score) {
                best = Some(Split {
                    dim,
                    score,
                    groups,
                    of_row,
                    distinct: sets.len(),
                });
            }
        }
        let Split {
            dim,
            groups,
            of_row,
            distinct,
            ..
        } = best?;
        let mut starts = vec![0usize; distinct + 1];
        for &slot in &of_row {
            starts[slot + 1] += 1;
        }
        for slot in 0..distinct {
            starts[slot + 1] += starts[slot];
        }
        let mut cursor = starts.clone();
        let mut by_slot = vec![0usize; rows.len()];
        for (&row, &slot) in rows.iter().zip(&of_row) {
            by_slot[cursor[slot]] = row;
            cursor[slot] += 1;
        }
        let split = groups
            .into_iter()
            .map(|(atom, ids)| {
                let mut atom_rows: Vec<usize> = ids
                    .into_iter()
                    .flat_map(|id| by_slot[starts[id]..starts[id + 1]].iter().copied())
                    .collect();
                atom_rows.sort_unstable();
                (atom, atom_rows)
            })
            .collect();
        Some((dim, split))
    }
}

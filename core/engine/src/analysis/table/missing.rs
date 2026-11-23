use std::sync::Arc;

use std::rc::Rc;

use ahash::{HashMap, RandomState};
use zen_expression::intellisense::values::cell::FieldPath;
use zen_expression::intellisense::{IntelliSense, ReadDependency};

use super::cell::CellConstraint;
use super::gaps::{Bits, Dense, Gaps, Space};
use super::intern::Interner;
use super::partition::Partition;
use super::value_set::{NumberSet, StringSet, ValueKind, ValueSet};
use super::verify::{Finding, GapAxis, GapTable, GapValues, VerifyTable};

const GAP_BUDGET: usize = 10_000_000;

pub(super) struct Dimension {
    pub(super) columns: Vec<usize>,
    pub(super) domain: ValueSet,
    pub(super) path: Option<Arc<str>>,
    pub(super) label: Arc<str>,
    pub(super) prefer: Option<ValueKind>,
    pub(super) dated: bool,
    pub(super) integer: bool,
    pub(super) input: bool,
}

struct Field {
    path: Option<Vec<Rc<str>>>,
    reads: Vec<Vec<Rc<str>>>,
}

impl Field {
    fn of(is: &mut IntelliSense, source: &str) -> Self {
        match FieldPath::of(is, source) {
            Some(path) => Self {
                reads: vec![path.clone()],
                path: Some(path),
            },
            None => Self {
                path: None,
                reads: is
                    .reads(source)
                    .into_iter()
                    .filter_map(|read| match read {
                        ReadDependency::Direct { path, .. } => Some(path),
                        ReadDependency::Iteration { collection, .. } => Some(collection),
                        _ => None,
                    })
                    .collect(),
            },
        }
    }

    fn overlaps(&self, other: &Field) -> bool {
        self.reads.iter().any(|a| {
            other
                .reads
                .iter()
                .any(|b| a.iter().zip(b).all(|(x, y)| x == y))
        })
    }
}

pub(super) enum Pool {
    Patterns {
        patterns: Interner<u64>,
        unions: HashMap<(u32, u32), u32>,
        joined: Vec<u64>,
    },
    Sets {
        sets: Vec<ValueSet>,
        index: HashMap<(u64, u32), u32>,
        unions: HashMap<(u32, u32), u32>,
    },
}

impl Pool {
    pub(super) fn sets(sets: Vec<ValueSet>, index: HashMap<(u64, u32), u32>) -> Self {
        Pool::Sets {
            sets,
            index,
            unions: HashMap::default(),
        }
    }

    fn patterns(patterns: Interner<u64>) -> Self {
        Pool::Patterns {
            patterns,
            unions: HashMap::default(),
            joined: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        match self {
            Pool::Patterns { patterns, .. } => patterns.len(),
            Pool::Sets { sets, .. } => sets.len(),
        }
    }

    fn is_empty(&self, value: u32) -> bool {
        match self {
            Pool::Patterns { .. } => false,
            Pool::Sets { sets, .. } => sets[value as usize].is_empty(),
        }
    }

    pub(super) fn intern(
        sets: &mut Vec<ValueSet>,
        index: &mut HashMap<(u64, u32), u32>,
        set: ValueSet,
    ) -> u32 {
        let hash = RandomState::with_seeds(11, 13, 17, 19).hash_one(&set);
        let mut probe = 0u32;
        loop {
            match index.get(&(hash, probe)) {
                Some(&id) if sets[id as usize] == set => return id,
                Some(_) => probe += 1,
                None => {
                    let id = sets.len() as u32;
                    index.insert((hash, probe), id);
                    sets.push(set);
                    return id;
                }
            }
        }
    }

    fn union(&mut self, a: u32, b: u32) -> u32 {
        match self {
            Pool::Patterns {
                patterns,
                unions,
                joined,
            } => {
                if let Some(&id) = unions.get(&(a, b)) {
                    return id;
                }
                joined.clear();
                joined.extend(
                    patterns
                        .get(a)
                        .iter()
                        .zip(patterns.get(b))
                        .map(|(left, right)| left | right),
                );
                let id = patterns.intern(joined);
                unions.insert((a, b), id);
                id
            }
            Pool::Sets {
                sets,
                index,
                unions,
            } => {
                if let Some(&id) = unions.get(&(a, b)) {
                    return id;
                }
                let joined = sets[a as usize].union(&sets[b as usize]);
                let id = Self::intern(sets, index, joined);
                unions.insert((a, b), id);
                id
            }
        }
    }

    fn keep(self, used: &[u32]) -> GapValues {
        match self {
            Pool::Patterns { patterns, .. } => GapValues::Patterns {
                words: patterns.stride(),
                data: used
                    .iter()
                    .flat_map(|&value| patterns.get(value).iter().copied())
                    .collect(),
            },
            Pool::Sets { mut sets, .. } => GapValues::Sets(
                used.iter()
                    .map(|&value| std::mem::take(&mut sets[value as usize]))
                    .collect(),
            ),
        }
    }
}

impl VerifyTable<'_> {
    pub(super) fn missing(
        &self,
        is: &mut IntelliSense,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
    ) -> Option<Option<Finding>> {
        if self.rules.is_empty() {
            return Some(None);
        }
        let dims = self.dimensions(is, cells);
        if dims.is_empty() || dims.iter().any(|d| d.domain.is_empty()) {
            return Some(None);
        }
        match self.missing_dense(dims, cells, satisfiable) {
            Err(dims) => self.missing_sets(dims, cells, satisfiable),
            Ok(found) => {
                #[cfg(test)]
                {
                    let cases = |found: &Option<Option<Finding>>| match found {
                        Some(Some(Finding::MissingCases { cases, total })) => {
                            Some(Some((cases.cases().collect::<Vec<_>>(), *total)))
                        }
                        Some(_) => Some(None),
                        None => None,
                    };
                    let expected =
                        self.missing_sets(self.dimensions(is, cells), cells, satisfiable);
                    assert_eq!(
                        cases(&found),
                        cases(&expected),
                        "gap analysis diverged between the dense and set-based paths"
                    );
                }
                found
            }
        }
    }

    fn gap_axis(&self, dim: Dimension, atoms: Vec<ValueSet>, values: GapValues) -> GapAxis {
        let first = dim.columns.first().map(|&first| &self.inputs[first]);
        GapAxis {
            label: dim.label,
            column: first.map(|input| input.id.clone()),
            subject: first
                .and_then(|input| input.subject.as_ref())
                .map(|subject| subject.join(".")),
            path: dim.path,
            input: dim.input,
            dated: dim.dated,
            prefer: dim.prefer,
            domain: dim.domain,
            atoms,
            values,
        }
    }

    fn missing_dense(
        &self,
        dims: Vec<Dimension>,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
    ) -> Result<Option<Option<Finding>>, Vec<Dimension>> {
        let rows: Vec<&Vec<CellConstraint>> = cells
            .iter()
            .zip(satisfiable)
            .filter(|(_, satisfiable)| **satisfiable)
            .map(|(row_cells, _)| row_cells)
            .collect();

        let atoms = dims.iter().map(|dim| {
            let mentioned = rows.iter().flat_map(|row_cells| {
                dim.columns.iter().filter_map(|&col| match &row_cells[col] {
                    CellConstraint::Known(set) => Some(set),
                    _ => None,
                })
            });
            Dense::atoms(mentioned.chain(std::iter::once(&dim.domain)))
        });
        if !Dense::fits(rows.len(), atoms) {
            return Err(dims);
        }

        let mut cut_of_row: Vec<Vec<u32>> = Vec::with_capacity(dims.len());
        let mut cut_sets: Vec<Vec<ValueSet>> = Vec::with_capacity(dims.len());
        for dim in &dims {
            let mut index: HashMap<ValueSet, u32> = HashMap::default();
            let mut distinct: Vec<ValueSet> = Vec::new();
            let of_row: Vec<u32> = rows
                .iter()
                .map(|row_cells| {
                    let cut =
                        dim.columns.iter().fold(ValueSet::all(), |acc, &col| {
                            match &row_cells[col] {
                                CellConstraint::Known(set) => acc.intersect(set),
                                _ => acc,
                            }
                        });
                    match index.get(&cut) {
                        Some(&id) => id,
                        None => {
                            let id = distinct.len() as u32;
                            index.insert(cut.clone(), id);
                            distinct.push(cut);
                            id
                        }
                    }
                })
                .collect();
            distinct.push(dim.domain.clone());
            cut_of_row.push(of_row);
            cut_sets.push(distinct);
        }
        let mut partitions: Vec<Partition> = Vec::with_capacity(dims.len());
        let mut cut_atoms: Vec<Vec<Vec<usize>>> = Vec::with_capacity(dims.len());
        for (dim, sets) in dims.iter().zip(&cut_sets) {
            let partition = Partition::new(&dim.domain, sets);
            cut_atoms.push(
                sets[..sets.len() - 1]
                    .iter()
                    .map(|cut| partition.touching(cut))
                    .collect(),
            );
            partitions.push(partition);
        }
        drop(cut_sets);

        let space = Space::new(partitions.iter().map(Partition::layout).collect());
        let stride = space.stride();
        let mut cuts = vec![0u64; rows.len() * stride];
        for ((axis, of_row), atoms) in space.axes().iter().zip(&cut_of_row).zip(&cut_atoms) {
            for (row, &id) in of_row.iter().enumerate() {
                let start = row * stride;
                let words = &mut cuts[start..start + stride][axis.range()];
                atoms[id as usize]
                    .iter()
                    .for_each(|&atom| Bits::set(words, atom));
            }
        }
        drop(cut_of_row);
        drop(cut_atoms);

        let Some(found) = Gaps::new(&space, &cuts, GAP_BUDGET).run(rows.len()) else {
            return Ok(None);
        };
        drop(cuts);

        let width = dims.len();
        let mut pools: Vec<Pool> = Vec::with_capacity(width);
        let mut value_of: Vec<Option<Vec<u32>>> = Vec::with_capacity(width);
        for ((dim, partition), patterns) in dims.iter().zip(&partitions).zip(found.patterns) {
            if !dim.integer {
                pools.push(Pool::patterns(patterns));
                value_of.push(None);
                continue;
            }
            let mut sets: Vec<ValueSet> = Vec::new();
            let mut index: HashMap<(u64, u32), u32> = HashMap::default();
            let atoms = partition.atoms();
            let values = (0..patterns.len() as u32)
                .map(|pattern| {
                    let mut set = Partition::union(
                        Bits::ones(patterns.get(pattern)).map(|atom| &atoms[atom]),
                    );
                    set.numbers = set.numbers.integral();
                    Pool::intern(&mut sets, &mut index, set)
                })
                .collect();
            pools.push(Pool::sets(sets, index));
            value_of.push(Some(values));
        }

        let mut remaining: Vec<u32> = Vec::with_capacity(found.fragments.len());
        for fragment in found.fragments.chunks_exact(width) {
            let start = remaining.len();
            remaining.extend(fragment.iter().zip(&value_of).map(
                |(&pattern, values)| match values {
                    Some(values) => values[pattern as usize],
                    None => pattern,
                },
            ));
            let empty = remaining[start..]
                .iter()
                .zip(&pools)
                .any(|(&value, pool)| pool.is_empty(value));
            if empty {
                remaining.truncate(start);
            }
        }
        drop(found.fragments);
        drop(value_of);
        if remaining.is_empty() {
            return Ok(Some(None));
        }

        let atoms = partitions.into_iter().map(Partition::into_atoms).collect();
        Ok(Some(Some(
            self.missing_cases_of(dims, atoms, pools, remaining),
        )))
    }

    pub(super) fn missing_cases_of(
        &self,
        dims: Vec<Dimension>,
        atoms: Vec<Vec<ValueSet>>,
        mut pools: Vec<Pool>,
        remaining: Vec<u32>,
    ) -> Finding {
        let width = dims.len();
        let mut rows = Self::merge(remaining, &mut pools);
        let total = rows.len() / width;
        let mut axes: Vec<GapAxis> = Vec::with_capacity(width);
        for (index, ((dim, atoms), pool)) in dims.into_iter().zip(atoms).zip(pools).enumerate() {
            let mut renamed = vec![u32::MAX; pool.len()];
            let mut used: Vec<u32> = Vec::new();
            for row in rows.chunks_exact_mut(width) {
                let value = row[index] as usize;
                if renamed[value] == u32::MAX {
                    renamed[value] = used.len() as u32;
                    used.push(value as u32);
                }
                row[index] = renamed[value];
            }
            axes.push(self.gap_axis(dim, atoms, pool.keep(&used)));
        }
        let cases = Rc::new(GapTable { axes, rows });
        Finding::MissingCases { cases, total }
    }

    pub(super) fn dimensions(
        &self,
        is: &mut IntelliSense,
        cells: &[Vec<CellConstraint>],
    ) -> Vec<Dimension> {
        let fields: Vec<Option<Field>> = self
            .inputs
            .iter()
            .map(|col| {
                col.field
                    .as_ref()
                    .filter(|_| col.analyzable)
                    .map(|field| Field::of(is, field))
            })
            .collect();
        let mut dims: Vec<(Vec<Rc<str>>, Dimension)> = Vec::new();
        for (idx, col) in self.inputs.iter().enumerate() {
            let (Some(field), Some(source)) = (&fields[idx], &col.field) else {
                continue;
            };
            let key = match &field.path {
                Some(path) => path.clone(),
                None if fields.iter().enumerate().any(|(other, f)| {
                    other != idx && f.as_ref().is_some_and(|f| f.overlaps(field))
                }) =>
                {
                    continue
                }
                None => vec![Rc::from(source.as_ref())],
            };
            let Some(domain) = col
                .domain
                .clone()
                .or_else(|| Self::derived_domain(cells, idx))
            else {
                continue;
            };
            match dims.iter_mut().find(|(existing, _)| *existing == key) {
                Some((_, dim)) => {
                    dim.columns.push(idx);
                    dim.domain = dim.domain.intersect(&domain);
                    dim.integer |= col.integer;
                    dim.path = dim.path.clone().or_else(|| col.path.clone());
                }
                None => dims.push((
                    key,
                    Dimension {
                        columns: vec![idx],
                        domain,
                        path: col.path.clone().or_else(|| {
                            field
                                .path
                                .as_ref()
                                .filter(|path| path.iter().all(|segment| !segment.contains('.')))
                                .map(|path| Arc::from(path.join(".")))
                        }),
                        label: col.label.clone(),
                        prefer: col.prefer,
                        dated: col.dated,
                        integer: col.integer,
                        input: col.input,
                    },
                )),
            }
        }
        dims.into_iter().map(|(_, dim)| dim).collect()
    }

    fn derived_domain(cells: &[Vec<CellConstraint>], col: usize) -> Option<ValueSet> {
        let mut domain = ValueSet::empty();
        for row in cells {
            let CellConstraint::Known(set) = &row[col] else {
                continue;
            };
            if !set.numbers.is_empty() {
                domain.numbers = NumberSet::all();
            }
            if !set.strings.is_empty() {
                domain.strings = StringSet::all();
            }
            if set.bools != 0 {
                domain.bools = ValueSet::TRUE | ValueSet::FALSE;
            }
        }
        (!domain.is_empty()).then_some(domain)
    }

    fn merge(mut fragments: Vec<u32>, pools: &mut [Pool]) -> Vec<u32> {
        let width = pools.len();
        let mut keys: Interner<u32> = Interner::new(width - 1);
        let mut key: Vec<u32> = Vec::with_capacity(width);
        loop {
            let mut changed = false;
            for dim in (0..width).rev() {
                keys.reset(width - 1);
                let mut merged: Vec<u32> = Vec::with_capacity(fragments.len());
                for fragment in fragments.chunks_exact(width) {
                    key.clear();
                    key.extend_from_slice(&fragment[..dim]);
                    key.extend_from_slice(&fragment[dim + 1..]);
                    let known = keys.len() as u32;
                    let slot = keys.intern(&key) as usize;
                    if slot as u32 == known {
                        merged.extend_from_slice(fragment);
                        continue;
                    }
                    let at = slot * width + dim;
                    merged[at] = pools[dim].union(merged[at], fragment[dim]);
                    changed = true;
                }
                fragments = merged;
            }
            if !changed {
                return fragments;
            }
        }
    }
}

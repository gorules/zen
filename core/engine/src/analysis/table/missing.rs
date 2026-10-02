use std::sync::Arc;

use std::rc::Rc;

use ahash::HashMap;
use serde_json::{Map, Value};
use zen_expression::intellisense::values::cell::FieldPath;
use zen_expression::intellisense::{IntelliSense, ReadDependency};

use super::cell::CellConstraint;
use super::partition::Partition;
use super::print::{CellText, DateDay};
use super::value_set::{NumberSet, StringSet, ValueKind, ValueSet};
use super::verify::{Finding, GapCase, Region, VerifyTable, MAX_FRAGMENTS};

const GAP_BUDGET: usize = 10_000_000;
const DIRECT_ROWS: usize = 8;

struct Dimension {
    columns: Vec<usize>,
    domain: ValueSet,
    path: Option<Arc<str>>,
    label: Arc<str>,
    prefer: Option<ValueKind>,
    dated: bool,
    integer: bool,
    input: bool,
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
        if remaining.is_empty() {
            return Some(None);
        }
        let remaining = Self::merge(remaining);
        let total = remaining.len();
        let cases = remaining
            .iter()
            .map(|fragment| self.case(&dims, fragment))
            .collect();
        Some(Some(Finding::MissingCases { cases, total }))
    }

    fn dimensions(&self, is: &mut IntelliSense, cells: &[Vec<CellConstraint>]) -> Vec<Dimension> {
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

    fn merge(mut fragments: Vec<Region>) -> Vec<Region> {
        let dims = fragments.first().map_or(0, Vec::len);
        loop {
            let mut changed = false;
            for dim in (0..dims).rev() {
                let mut buckets: HashMap<Vec<ValueSet>, usize> = HashMap::default();
                let mut merged: Vec<Region> = Vec::with_capacity(fragments.len());
                for fragment in fragments.drain(..) {
                    let key: Vec<ValueSet> = fragment
                        .iter()
                        .enumerate()
                        .filter(|(idx, _)| *idx != dim)
                        .map(|(_, set)| set.clone())
                        .collect();
                    match buckets.get(&key) {
                        Some(&idx) => {
                            merged[idx][dim] = merged[idx][dim].union(&fragment[dim]);
                            changed = true;
                        }
                        None => {
                            buckets.insert(key, merged.len());
                            merged.push(fragment);
                        }
                    }
                }
                fragments = merged;
            }
            if !changed {
                return fragments;
            }
        }
    }

    fn case(&self, dims: &[Dimension], fragment: &Region) -> GapCase {
        let mut parts = Vec::new();
        let mut cells = Some(Vec::new());
        let mut example = Some(Map::new());
        for (dim, set) in dims.iter().zip(fragment) {
            match CellText::of(set, &dim.domain, dim.dated) {
                Some(text) if text.is_empty() => continue,
                Some(text) => {
                    if !dim.input {
                        example = None;
                    }
                    if let Some(cells) = cells.as_mut() {
                        if let Some(&first) = dim.columns.first() {
                            cells.push((self.inputs[first].id.clone(), text.clone()));
                        }
                    }
                    parts.push((dim.label.clone(), text));
                }
                None => {
                    cells = None;
                    parts.push((dim.label.clone(), "…".to_string()));
                }
            }
            let value = match dim.dated {
                true => DateDay::example(set),
                false => set.example(dim.prefer),
            };
            match (&dim.path, value, example.as_mut()) {
                (Some(path), Some(value), Some(root)) => Self::insert(root, path, value),
                _ => example = None,
            }
        }
        GapCase {
            parts,
            cells,
            example: example.filter(|root| !root.is_empty()).map(Value::Object),
        }
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
            let groups = Partition::groups(bounds, &sets);
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
        let split = groups
            .into_iter()
            .map(|(atom, ids)| {
                let mut touching = vec![false; distinct];
                for id in ids {
                    touching[id] = true;
                }
                let atom_rows = rows
                    .iter()
                    .zip(&of_row)
                    .filter(|(_, slot)| touching[**slot])
                    .map(|(&row, _)| row)
                    .collect();
                (atom, atom_rows)
            })
            .collect();
        Some((dim, split))
    }
}

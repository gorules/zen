use std::sync::Arc;

use ahash::HashMap;
use serde_json::{Map, Value};

use super::cell::CellConstraint;
use super::print::{CellText, DateDay};
use super::value_set::{NumberSet, StringSet, ValueKind, ValueSet};
use super::verify::{Budget, Finding, GapCase, Region, VerifyTable};
use super::FullCheck;

const GAP_BUDGET: usize = 2_000_000;

struct Dimension {
    columns: Vec<usize>,
    domain: ValueSet,
    path: Option<Arc<str>>,
    label: Arc<str>,
    prefer: Option<ValueKind>,
    dated: bool,
    input: bool,
}

impl VerifyTable<'_> {
    pub(super) fn missing(
        &self,
        cells: &[Vec<CellConstraint>],
        satisfiable: &[bool],
    ) -> Option<Option<Finding>> {
        if self.rules.is_empty() {
            return Some(None);
        }
        let dims = self.dimensions(cells);
        if dims.is_empty() || dims.iter().any(|d| d.domain.is_empty()) {
            return Some(None);
        }
        let mut remaining: Vec<Region> = vec![dims.iter().map(|d| d.domain.clone()).collect()];
        let mut budget = Budget {
            remaining: FullCheck::scale(GAP_BUDGET),
        };
        for (row, row_cells) in cells.iter().enumerate() {
            if !satisfiable[row] {
                continue;
            }
            let cut: Region = dims
                .iter()
                .map(|d| {
                    d.columns
                        .iter()
                        .fold(ValueSet::all(), |acc, &col| match &row_cells[col] {
                            CellConstraint::Known(set) => acc.intersect(set),
                            _ => acc,
                        })
                })
                .collect();
            let mut next = Vec::with_capacity(remaining.len());
            for fragment in remaining {
                budget.spend(fragment.len())?;
                match Self::subtract(&fragment, &cut) {
                    Some(pieces) => next.extend(pieces),
                    None => next.push(fragment),
                }
            }
            if next.len() > Self::max_fragments() {
                return None;
            }
            remaining = next;
            if remaining.is_empty() {
                return Some(None);
            }
        }
        let remaining = Self::merge(remaining);
        let total = remaining.len();
        let cases = remaining
            .iter()
            .map(|fragment| self.case(&dims, fragment))
            .collect();
        Some(Some(Finding::MissingCases { cases, total }))
    }

    fn dimensions(&self, cells: &[Vec<CellConstraint>]) -> Vec<Dimension> {
        let mut dims: Vec<(Arc<str>, Dimension)> = Vec::new();
        for (idx, col) in self.inputs.iter().enumerate() {
            if !col.analyzable {
                continue;
            }
            let Some(field) = col.field.clone() else {
                continue;
            };
            let Some(domain) = col
                .domain
                .clone()
                .or_else(|| Self::derived_domain(cells, idx))
            else {
                continue;
            };
            match dims.iter_mut().find(|(key, _)| *key == field) {
                Some((_, dim)) => {
                    dim.columns.push(idx);
                    dim.domain = dim.domain.intersect(&domain);
                }
                None => dims.push((
                    field,
                    Dimension {
                        columns: vec![idx],
                        domain,
                        path: col.path.clone(),
                        label: col.label.clone(),
                        prefer: col.prefer,
                        dated: col.dated,
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

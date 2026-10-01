use std::rc::Rc;
use std::sync::Arc;

use ahash::HashMap;
use serde_json::{Map, Value};
use zen_expression::intellisense::IntelliSense;

use super::cell::CellConstraint;
use super::index::RowIndex;
use super::print::DateDay;
use super::value_set::{ValueKind, ValueSet};

pub(crate) const MAX_INPUTS: usize = 30;
pub(super) const MAX_FRAGMENTS: usize = 100_000;
const TOTAL_BUDGET: usize = 20_000_000;
const MAX_MINIMIZE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HitMode {
    PerColumnFirst,
    RowFirst,
    Collect,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifyInput {
    pub(crate) id: Arc<str>,
    pub(crate) unary: bool,
    pub(crate) analyzable: bool,
    pub(crate) dated: bool,
    pub(crate) input: bool,
    pub(crate) field: Option<Arc<str>>,
    pub(crate) path: Option<Arc<str>>,
    pub(crate) prefer: Option<ValueKind>,
    pub(crate) label: Arc<str>,
    pub(crate) domain: Option<ValueSet>,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifyOutput {
    pub(crate) id: Arc<str>,
    pub(crate) collect: bool,
    pub(crate) label: Arc<str>,
    pub(crate) values: Option<Vec<Rc<str>>>,
}

pub(crate) struct VerifyTable<'a> {
    pub(crate) mode: HitMode,
    pub(crate) inputs: Vec<VerifyInput>,
    pub(crate) outputs: Vec<VerifyOutput>,
    pub(crate) rules: &'a [HashMap<Arc<str>, Arc<str>>],
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Finding {
    UnsatisfiableCell {
        row: usize,
        col: Arc<str>,
    },
    UnreachableRule {
        row: usize,
        covered_by: Vec<usize>,
        example: Option<Value>,
        same_conditions: bool,
        redundant: bool,
    },
    DuplicateRule {
        row: usize,
        of: usize,
        redundant: bool,
    },
    MissingCases {
        cases: Vec<GapCase>,
        total: usize,
    },
    CompressibleTable {
        before: usize,
        rules: Vec<HashMap<Arc<str>, Arc<str>>>,
    },
    CellCoversDomain {
        row: usize,
        col: Arc<str>,
    },
    OutputNeverProduced {
        col: Arc<str>,
        values: Vec<Rc<str>>,
    },
    ChecksIncomplete {
        rows: usize,
        coverage: bool,
        gaps: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GapCase {
    pub(crate) parts: Vec<(Arc<str>, String)>,
    pub(crate) cells: Option<Vec<(Arc<str>, String)>>,
    pub(crate) example: Option<Value>,
}

pub(super) type Region = Vec<ValueSet>;

type Coverer<'c> = (usize, Vec<&'c ValueSet>);

pub(super) struct Budget {
    pub(super) remaining: usize,
}

impl Budget {
    pub(super) fn spend(&mut self, amount: usize) -> Option<()> {
        self.remaining = self.remaining.checked_sub(amount)?;
        Some(())
    }
}

impl VerifyTable<'_> {
    pub(crate) fn verify(&self, is: &mut IntelliSense) -> Vec<Finding> {
        let mut findings = Vec::new();
        if self.inputs.is_empty() {
            return findings;
        }
        let cells: Vec<Vec<CellConstraint>> = self
            .rules
            .iter()
            .map(|rule| {
                self.inputs
                    .iter()
                    .map(|col| {
                        let source = rule.get(&col.id).map(|c| c.as_ref()).unwrap_or("");
                        CellConstraint::parse(is, source, col.unary, col.analyzable, col.dated)
                    })
                    .collect()
            })
            .collect();

        let mut satisfiable = vec![true; self.rules.len()];
        for (row, row_cells) in cells.iter().enumerate() {
            for (col, cell) in self.inputs.iter().zip(row_cells) {
                if matches!(cell, CellConstraint::Known(set) if set.is_empty()) {
                    satisfiable[row] = false;
                    findings.push(Finding::UnsatisfiableCell {
                        row,
                        col: col.id.clone(),
                    });
                }
            }
        }

        let coverage = self.inputs.len() <= MAX_INPUTS;
        let mut coverage_incomplete = !coverage;

        let mut budget = Budget {
            remaining: TOTAL_BUDGET,
        };
        let mut index =
            (self.mode != HitMode::Collect).then(|| RowIndex::new(&cells, self.inputs.len()));
        let mut reported: Vec<bool> = satisfiable.iter().map(|s| !s).collect();
        let mut seen: HashMap<(Vec<CellConstraint>, Vec<String>), usize> = HashMap::default();
        for row in 0..self.rules.len() {
            if let Some(index) = index.as_mut().filter(|_| row > 0) {
                if satisfiable[row - 1] && !reported[row - 1] {
                    index.insert(&cells[row - 1], row - 1);
                }
            }
            if !satisfiable[row] {
                continue;
            }
            match seen.entry(self.row_signature(&cells[row], row)) {
                std::collections::hash_map::Entry::Occupied(first) => {
                    let of = *first.get();
                    reported[row] = true;
                    let redundant = self.mode != HitMode::Collect && !self.contributes_collect(row);
                    findings.push(Finding::DuplicateRule { row, of, redundant });
                    continue;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(row);
                }
            }
            if coverage_incomplete {
                continue;
            }
            let Some(index) = index.as_mut() else {
                continue;
            };
            let Some(requirements) = self.requirements(row, || index.candidates(&cells, row))
            else {
                continue;
            };
            match self.dead(&cells, row, &requirements, &mut budget, index) {
                Some(Some(covered_by)) => {
                    reported[row] = true;
                    let redundant = covered_by
                        .iter()
                        .all(|&earlier| self.same_row_outputs(earlier, row));
                    let same_conditions = !redundant
                        && covered_by.len() == 1
                        && self.row_signature(&cells[row], row).0
                            == self.row_signature(&cells[covered_by[0]], covered_by[0]).0;
                    findings.push(Finding::UnreachableRule {
                        row,
                        example: self.example(&cells[row]),
                        covered_by,
                        same_conditions,
                        redundant,
                    });
                }
                Some(None) => {}
                None => coverage_incomplete = true,
            }
        }
        let mut gaps_incomplete = !coverage;
        if coverage {
            match self.missing(&cells, &satisfiable) {
                Some(Some(missing)) => findings.push(missing),
                Some(None) => {}
                None => gaps_incomplete = true,
            }
        }
        findings.extend(self.compress(&cells, &satisfiable));
        findings.extend(self.covering_cells(&cells, &satisfiable));
        findings.extend(self.unproduced(is, &reported));
        if coverage_incomplete || gaps_incomplete {
            findings.push(Finding::ChecksIncomplete {
                rows: self.rules.len(),
                coverage: coverage_incomplete,
                gaps: gaps_incomplete,
            });
        }
        findings
    }

    fn unproduced(&self, is: &mut IntelliSense, reported: &[bool]) -> Vec<Finding> {
        let mut findings = Vec::new();
        for col in &self.outputs {
            let Some(values) = &col.values else {
                continue;
            };
            let mut produced: Vec<String> = Vec::new();
            let mut provable = true;
            for row in (0..self.rules.len()).filter(|&row| !reported[row]) {
                let text = self.text(row, &col.id);
                if text.is_empty() {
                    continue;
                }
                match is.with_ast(text, false, |node, _| match node {
                    zen_expression::parser::Node::String(s) => Some(s.to_string()),
                    _ => None,
                }) {
                    Some(Some(value)) => produced.push(value),
                    _ => {
                        provable = false;
                        break;
                    }
                }
            }
            if !provable || produced.is_empty() {
                continue;
            }
            let missing: Vec<Rc<str>> = values
                .iter()
                .filter(|v| !produced.iter().any(|p| p.as_str() == v.as_ref()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                findings.push(Finding::OutputNeverProduced {
                    col: col.id.clone(),
                    values: missing,
                });
            }
        }
        findings
    }

    fn contributes_collect(&self, row: usize) -> bool {
        self.outputs
            .iter()
            .any(|col| col.collect && self.filled(row, &col.id))
    }

    fn same_row_outputs(&self, a: usize, b: usize) -> bool {
        self.outputs
            .iter()
            .all(|col| self.text(a, &col.id) == self.text(b, &col.id))
    }

    pub(super) fn filled(&self, row: usize, id: &Arc<str>) -> bool {
        self.rules[row]
            .get(id)
            .is_some_and(|c| !c.trim().is_empty())
    }

    pub(super) fn text<'r>(&'r self, row: usize, id: &Arc<str>) -> &'r str {
        self.rules[row].get(id).map(|c| c.trim()).unwrap_or("")
    }

    fn row_signature(
        &self,
        cells: &[CellConstraint],
        row: usize,
    ) -> (Vec<CellConstraint>, Vec<String>) {
        let cells = cells
            .iter()
            .map(|cell| match cell {
                CellConstraint::Known(set) if set.is_all() => CellConstraint::Any,
                other => other.clone(),
            })
            .collect();
        let outputs = self
            .outputs
            .iter()
            .map(|col| self.text(row, &col.id).to_string())
            .collect();
        (cells, outputs)
    }

    fn requirements(
        &self,
        row: usize,
        candidates: impl FnOnce() -> Vec<usize>,
    ) -> Option<Vec<Vec<usize>>> {
        let has_collect = self
            .outputs
            .iter()
            .any(|col| col.collect && self.filled(row, &col.id));
        match self.mode {
            HitMode::Collect => None,
            _ if has_collect => None,
            HitMode::RowFirst => Some(vec![candidates()]),
            HitMode::PerColumnFirst => {
                let scalars: Vec<&VerifyOutput> = self
                    .outputs
                    .iter()
                    .filter(|col| !col.collect && self.filled(row, &col.id))
                    .collect();
                if scalars.is_empty() {
                    return None;
                }
                let candidates = candidates();
                Some(
                    scalars
                        .into_iter()
                        .map(|col| {
                            candidates
                                .iter()
                                .copied()
                                .filter(|&e| self.filled(e, &col.id))
                                .collect()
                        })
                        .collect(),
                )
            }
        }
    }

    fn dead(
        &self,
        cells: &[Vec<CellConstraint>],
        row: usize,
        requirements: &[Vec<usize>],
        budget: &mut Budget,
        index: &RowIndex,
    ) -> Option<Option<Vec<usize>>> {
        let region: Region = cells[row]
            .iter()
            .map(|cell| cell.known_set().unwrap_or_else(ValueSet::all))
            .collect();
        for earlier in requirements {
            budget.spend(earlier.len())?;
            if Self::escapes(cells, row, &region, earlier, index) {
                return Some(None);
            }
        }
        let all = ValueSet::all();
        let mut cited: Vec<usize> = Vec::new();
        for earlier in requirements {
            let coverers = earlier
                .iter()
                .filter_map(|&e| Self::project(&cells[e], &cells[row], &all).map(|r| (e, r)));
            let Some(used) = Self::cover(&region, coverers, budget)? else {
                return Some(None);
            };
            cited.extend(Self::minimize(&region, used, budget)?);
        }
        cited.sort_unstable();
        cited.dedup();
        Some(Some(cited))
    }

    fn project<'c>(
        earlier: &'c [CellConstraint],
        row: &[CellConstraint],
        all: &'c ValueSet,
    ) -> Option<Vec<&'c ValueSet>> {
        earlier
            .iter()
            .zip(row)
            .map(|(e, r)| match e {
                CellConstraint::Any => Some(all),
                CellConstraint::Known(set) => (!set.is_empty()).then_some(set),
                CellConstraint::Opaque(atom) => match r {
                    CellConstraint::Opaque(own) if own == atom => Some(all),
                    _ => None,
                },
            })
            .collect()
    }

    fn cover<'c>(
        region: &Region,
        coverers: impl IntoIterator<Item = Coverer<'c>>,
        budget: &mut Budget,
    ) -> Option<Option<Vec<Coverer<'c>>>> {
        let mut remaining: Vec<Region> = vec![region.clone()];
        let mut used = Vec::new();
        for (idx, cut) in coverers {
            let mut next = Vec::with_capacity(remaining.len());
            let mut touched = false;
            for fragment in remaining {
                budget.spend(fragment.len())?;
                match Self::subtract(&fragment, &cut) {
                    Some(pieces) => {
                        touched = true;
                        next.extend(pieces);
                    }
                    None => next.push(fragment),
                }
            }
            if next.len() > MAX_FRAGMENTS {
                return None;
            }
            if touched {
                used.push((idx, cut));
            }
            remaining = next;
            if remaining.is_empty() {
                return Some(Some(used));
            }
        }
        Some(None)
    }

    fn minimize(region: &Region, used: Vec<Coverer>, budget: &mut Budget) -> Option<Vec<usize>> {
        if used.len() > MAX_MINIMIZE {
            return Some(used.into_iter().map(|(idx, _)| idx).collect());
        }
        let mut kept = used;
        let mut i = kept.len();
        while i > 0 {
            i -= 1;
            let candidate = kept
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, (idx, cut))| (*idx, cut.clone()));
            if Self::cover(region, candidate, budget)?.is_some() {
                kept.remove(i);
            }
        }
        Some(kept.into_iter().map(|(idx, _)| idx).collect())
    }

    pub(super) fn subtract(fragment: &Region, cut: &[&ValueSet]) -> Option<Vec<Region>> {
        if fragment.iter().zip(cut).any(|(f, c)| !f.intersects(c)) {
            return None;
        }
        let mut pieces = Vec::new();
        let mut prefix = fragment.clone();
        for dim in 0..fragment.len() {
            let outside = prefix[dim].difference(cut[dim]);
            if !outside.is_empty() {
                let mut piece = prefix.clone();
                piece[dim] = outside;
                pieces.push(piece);
            }
            prefix[dim] = prefix[dim].intersect(cut[dim]);
        }
        Some(pieces)
    }

    fn example(&self, cells: &[CellConstraint]) -> Option<Value> {
        let mut by_path: Vec<(Arc<str>, ValueSet, Option<ValueKind>, bool)> = Vec::new();
        for (col, cell) in self.inputs.iter().zip(cells) {
            if !col.input && !matches!(cell, CellConstraint::Any) {
                return None;
            }
            let set = match cell {
                CellConstraint::Opaque(_) => return None,
                CellConstraint::Any => match &col.path {
                    Some(_) => ValueSet::all(),
                    None => continue,
                },
                CellConstraint::Known(set) => set.clone(),
            };
            let path = col.path.clone()?;
            match by_path.iter_mut().find(|(p, _, _, _)| *p == path) {
                Some((_, existing, _, _)) => *existing = existing.intersect(&set),
                None => by_path.push((path, set, col.prefer, col.dated)),
            }
        }
        let mut root = Map::new();
        for (path, set, prefer, dated) in by_path {
            if set.is_all() {
                continue;
            }
            let value = match dated {
                true => DateDay::example(&set)?,
                false => set.example(prefer)?,
            };
            Self::insert(&mut root, &path, value);
        }
        (!root.is_empty()).then_some(Value::Object(root))
    }

    pub(super) fn insert(root: &mut Map<String, Value>, path: &str, value: Value) {
        let mut segments = path.split('.').peekable();
        let mut node = root;
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                node.insert(segment.to_string(), value);
                return;
            }
            let entry = node
                .entry(segment.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            let Value::Object(next) = entry else {
                return;
            };
            node = next;
        }
    }
}

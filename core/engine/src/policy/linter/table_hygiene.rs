use std::sync::Arc;

use ahash::HashSet;

use crate::policy::blocks::{BlockKind, DecisionTableIr};
use crate::policy::types::{Diagnostic, DiagnosticCode, DiagnosticLocation};

use super::{LintContext, LintRule};

pub(crate) struct RedundantTableRow;

pub(crate) struct NonDiscriminatingColumn;

struct TableView {
    inputs: Vec<(Arc<str>, Arc<str>)>,
    rows: Vec<RowView>,
}

struct RowView {
    inputs: Vec<String>,
    outputs: Vec<String>,
}

impl TableView {
    fn first_hit(table: &DecisionTableIr) -> Option<Self> {
        if table.outputs.is_empty() || table.outputs.iter().any(|o| o.collect) {
            return None;
        }
        let inputs: Vec<(Arc<str>, Arc<str>)> = table
            .inputs
            .iter()
            .map(|col| {
                let label = if col.name.is_empty() {
                    col.field.clone().unwrap_or_else(|| Arc::from(""))
                } else {
                    col.name.clone()
                };
                (col.id.clone(), label)
            })
            .collect();
        if inputs.is_empty() {
            return None;
        }
        let rows = table
            .rules
            .iter()
            .map(|rule| RowView {
                inputs: inputs.iter().map(|(id, _)| Self::cell(rule, id)).collect(),
                outputs: table
                    .outputs
                    .iter()
                    .map(|col| Self::cell(rule, &col.id))
                    .collect(),
            })
            .collect();
        Some(Self { inputs, rows })
    }

    fn cell(rule: &ahash::HashMap<Arc<str>, Arc<str>>, id: &Arc<str>) -> String {
        rule.get(id)
            .map(|c| c.trim().to_string())
            .unwrap_or_default()
    }

    fn shadows(earlier: &RowView, later: &RowView) -> bool {
        earlier
            .inputs
            .iter()
            .zip(&later.inputs)
            .all(|(e, l)| e.is_empty() || e == l)
    }
}

impl LintRule for RedundantTableRow {
    fn check(&self, cx: &LintContext, out: &mut Vec<Diagnostic>) {
        for block in cx.rules() {
            let BlockKind::DecisionTable(table) = &block.kind else {
                continue;
            };
            let Some(view) = TableView::first_hit(table) else {
                continue;
            };

            for later_idx in 1..view.rows.len() {
                let later = &view.rows[later_idx];
                let Some(earlier_idx) =
                    (0..later_idx).find(|&i| TableView::shadows(&view.rows[i], later))
                else {
                    continue;
                };
                let earlier = &view.rows[earlier_idx];
                let message = if earlier.inputs == later.inputs && earlier.outputs == later.outputs
                {
                    format!(
                        "row {} duplicates row {} — remove it",
                        later_idx + 1,
                        earlier_idx + 1
                    )
                } else {
                    format!(
                        "row {} is unreachable — row {} already matches every case it matches",
                        later_idx + 1,
                        earlier_idx + 1
                    )
                };
                out.push(Diagnostic::hint(
                    DiagnosticCode::RedundantTableRow,
                    DiagnosticLocation::block(cx.target().clone(), block.id.clone()),
                    message,
                ));
            }
        }
    }
}

impl LintRule for NonDiscriminatingColumn {
    fn check(&self, cx: &LintContext, out: &mut Vec<Diagnostic>) {
        for block in cx.rules() {
            let BlockKind::DecisionTable(table) = &block.kind else {
                continue;
            };
            let Some(view) = TableView::first_hit(table) else {
                continue;
            };
            if view.rows.len() < 2 {
                continue;
            }

            for (col_idx, (_, name)) in view.inputs.iter().enumerate() {
                let label = if name.is_empty() {
                    format!("#{}", col_idx + 1)
                } else {
                    format!("'{name}'")
                };
                if view.rows.iter().all(|r| r.inputs[col_idx].is_empty()) {
                    out.push(Diagnostic::hint(
                        DiagnosticCode::NonDiscriminatingColumn,
                        DiagnosticLocation::block(cx.target().clone(), block.id.clone()),
                        format!("input column {label} has no conditions — remove it"),
                    ));
                    continue;
                }
                if Self::never_affects_outcome(&view, col_idx) {
                    out.push(Diagnostic::hint(
                        DiagnosticCode::NonDiscriminatingColumn,
                        DiagnosticLocation::block(cx.target().clone(), block.id.clone()),
                        format!(
                            "input column {label} never changes the outcome — rows differing only in this column produce identical results; remove it and dedupe the rows"
                        ),
                    ));
                }
            }
        }
    }
}

impl NonDiscriminatingColumn {
    fn never_affects_outcome(view: &TableView, col_idx: usize) -> bool {
        let mut groups: Vec<(Vec<&str>, Vec<usize>)> = Vec::new();
        for (row_idx, row) in view.rows.iter().enumerate() {
            let key: Vec<&str> = row
                .inputs
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != col_idx)
                .map(|(_, cell)| cell.as_str())
                .collect();
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push(row_idx),
                None => groups.push((key, vec![row_idx])),
            }
        }

        let mut merges_anything = false;
        for (key, members) in &groups {
            let outputs = &view.rows[members[0]].outputs;
            if members
                .iter()
                .any(|&idx| view.rows[idx].outputs != *outputs)
            {
                return false;
            }
            if members.len() == 1 {
                if !view.rows[members[0]].inputs[col_idx].is_empty() {
                    return false;
                }
                continue;
            }
            let has_wildcard_member = members
                .iter()
                .any(|&idx| view.rows[idx].inputs[col_idx].is_empty());
            if !has_wildcard_member
                && !Self::fall_through_preserved(view, col_idx, key, members, outputs)
            {
                return false;
            }
            let distinct: HashSet<&str> = members
                .iter()
                .map(|&idx| view.rows[idx].inputs[col_idx].as_str())
                .collect();
            if distinct.len() > 1 {
                merges_anything = true;
            }
        }
        merges_anything
    }

    fn fall_through_preserved(
        view: &TableView,
        col_idx: usize,
        key: &[&str],
        members: &[usize],
        outputs: &[String],
    ) -> bool {
        for (idx, row) in view.rows.iter().enumerate() {
            let others_subsume = row
                .inputs
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != col_idx)
                .zip(key)
                .all(|((_, cell), k)| cell.is_empty() || cell.as_str() == *k);
            if !others_subsume {
                continue;
            }
            if row.inputs[col_idx].is_empty() {
                return row.outputs == outputs;
            }
            if !members.contains(&idx) {
                return false;
            }
        }
        false
    }
}

mod cache;
mod constraints;
mod index;
mod merge;
mod missing;
mod partition;
mod verify;
mod witness;

use zen_expression::intellisense::values::{cell, print, value_set};

use std::sync::Arc;

use zen_expression::intellisense::IntelliSense;
use zen_expression::variable::VariableType;

use crate::workspace::types::{
    CursorTarget, Diagnostic, DiagnosticArgs, DiagnosticCode, DiagnosticLocation,
};

pub(crate) use value_set::{Bound, Interval, NumberSet, ValueKind};
pub(crate) use verify::{HitMode, VerifyInput, VerifyOutput, VerifyTable};

pub(crate) use constraints::PathConstraints;
use serde_json::{Map, Value};
pub(crate) use value_set::ValueSet;

use print::CellText;
use value_set::StringSet;
use verify::{Finding, GapCase};

pub(crate) struct TableColumn;

impl TableColumn {
    pub(crate) fn output(
        id: &Arc<str>,
        field: &str,
        collect: bool,
        declared: Option<VariableType>,
    ) -> VerifyOutput {
        let label = Arc::from(field.trim());
        let values = match declared {
            Some(VariableType::Enum(_, values)) => Some(values),
            _ => None,
        };
        VerifyOutput {
            id: id.clone(),
            collect,
            label,
            values,
        }
    }
}

impl TableColumn {
    pub(crate) fn input(
        id: &Arc<str>,
        name: &Arc<str>,
        field: Option<&Arc<str>>,
        field_type: Option<&VariableType>,
    ) -> VerifyInput {
        let field = field.filter(|f| !f.trim().is_empty());
        let resolved = field_type.map(|t| t.unwrap_nullable().0);
        let dated = resolved.is_some_and(|t| matches!(t, VariableType::Date));
        let label = match (name.trim(), field) {
            ("", Some(f)) => Arc::from(f.trim()),
            (name, _) => Arc::from(name),
        };
        VerifyInput {
            id: id.clone(),
            unary: field.is_some(),
            analyzable: field.is_some(),
            dated,
            input: true,
            field: field.map(|f| Arc::from(f.trim())),
            path: field.filter(|f| Self::is_plain_path(f)).cloned(),
            prefer: resolved.and_then(Self::kind_of),
            label,
            domain: field_type.and_then(Self::domain_of),
        }
    }

    pub(crate) fn narrow(mut input: VerifyInput, allowed: &ValueSet) -> VerifyInput {
        let allowed = match input.dated {
            true => match Self::dated(allowed) {
                Some(allowed) => allowed,
                None => return input,
            },
            false => allowed.clone(),
        };
        if let Some(domain) = input.domain.as_mut() {
            *domain = domain.intersect(&allowed);
        }
        input
    }

    fn dated(allowed: &ValueSet) -> Option<ValueSet> {
        let StringSet::Finite(strings) = &allowed.strings else {
            return None;
        };
        let mut points = Vec::with_capacity(strings.len());
        for text in strings {
            points.push(Interval::point(print::DateDay::seconds(text)?));
        }
        Some(ValueSet {
            numbers: allowed.numbers.union(&NumberSet::from_intervals(points)),
            strings: StringSet::Finite(Default::default()),
            ..allowed.clone()
        })
    }

    pub(crate) fn narrow_numbers(mut input: VerifyInput, range: NumberSet) -> VerifyInput {
        if let Some(domain) = input.domain.as_mut() {
            domain.numbers = domain.numbers.intersect(&range);
        }
        input
    }

    fn domain_of(field_type: &VariableType) -> Option<ValueSet> {
        let (resolved, nullable) = field_type.unwrap_nullable();
        let mut domain = match resolved {
            VariableType::Number | VariableType::Date => ValueSet::numbers(NumberSet::all()),
            VariableType::String => ValueSet {
                strings: StringSet::all(),
                ..ValueSet::empty()
            },
            VariableType::Bool => ValueSet::bool(true).union(&ValueSet::bool(false)),
            VariableType::Enum(_, values) => values
                .iter()
                .fold(ValueSet::empty(), |acc, v| acc.union(&ValueSet::string(v))),
            VariableType::Const(value) => ValueSet::string(value),
            _ => return None,
        };
        if nullable {
            domain = domain.union(&ValueSet::null());
        }
        Some(domain)
    }

    fn kind_of(resolved: &VariableType) -> Option<ValueKind> {
        match resolved {
            VariableType::Number => Some(ValueKind::Number),
            VariableType::String | VariableType::Enum(..) | VariableType::Const(_) => {
                Some(ValueKind::String)
            }
            VariableType::Bool => Some(ValueKind::Bool),
            _ => None,
        }
    }

    fn is_plain_path(field: &str) -> bool {
        field.split('.').all(|segment| {
            let mut chars = segment.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    }
}

impl VerifyTable<'_> {
    pub(crate) fn diagnostics(
        &self,
        is: &mut IntelliSense,
        row_key: impl Fn(usize) -> Arc<str>,
        location: impl Fn(Option<Arc<str>>) -> DiagnosticLocation,
    ) -> Vec<Diagnostic> {
        self.cached_findings(is)
            .iter()
            .cloned()
            .map(|finding| self.diagnostic(finding, &row_key, &location))
            .collect()
    }

    fn diagnostic(
        &self,
        finding: Finding,
        row_key: &impl Fn(usize) -> Arc<str>,
        location: &impl Fn(Option<Arc<str>>) -> DiagnosticLocation,
    ) -> Diagnostic {
        let row_args = |row: usize| {
            DiagnosticArgs::from([
                ("row", (row + 1).to_string()),
                ("rowId", row_key(row).to_string()),
            ])
        };
        let row_target = |row: usize| CursorTarget::DecisionTableRow { row: row_key(row) };
        match finding {
            Finding::UnsatisfiableCell { row, col } => {
                let mut diagnostic = Diagnostic::warning(
                    DiagnosticCode::UnsatisfiableCell,
                    location(Some(col.clone())).with_target(CursorTarget::DecisionTableCell {
                        row: row_key(row),
                        col,
                    }),
                    format!(
                        "this condition can never match, so row {} never fires",
                        row + 1
                    ),
                );
                diagnostic.args = row_args(row);
                diagnostic
            }
            Finding::UnreachableRule {
                row,
                covered_by,
                example,
                same_conditions,
                redundant,
            } => {
                let rows = Self::row_list(&covered_by);
                let mut message = match covered_by.len() {
                    1 if redundant => format!(
                        "row {} is redundant: row {rows} already gives the same result for every input it matches",
                        row + 1
                    ),
                    _ if redundant => format!(
                        "row {} is redundant: rows {rows} already give the same result for every input it matches",
                        row + 1
                    ),
                    1 if same_conditions => format!(
                        "row {} has the same conditions as row {rows} but a different result; only row {rows} is used",
                        row + 1
                    ),
                    1 => format!(
                        "row {} never fires: row {rows} already matches every input it matches and returns a different result",
                        row + 1
                    ),
                    _ => format!(
                        "row {} never fires: rows {rows} together already match every input it matches, with a different result",
                        row + 1
                    ),
                };
                let mut args = row_args(row);
                args.insert("coveredBy", rows);
                args.insert(
                    "coveredByIds",
                    covered_by
                        .iter()
                        .map(|r| row_key(*r).to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                );
                args.insert("coveredCount", covered_by.len().to_string());
                if same_conditions {
                    args.insert("sameConditions", "true".to_string());
                }
                if redundant {
                    args.insert("redundant", "true".to_string());
                }
                if let Some(example) = example {
                    message.push_str(&format!(", for example {example}"));
                    args.insert("example", example.to_string());
                }
                let mut diagnostic = (if redundant {
                    Diagnostic::hint
                } else {
                    Diagnostic::warning
                })(
                    DiagnosticCode::UnreachableRule,
                    location(None).with_target(row_target(row)),
                    message,
                );
                diagnostic.args = args;
                diagnostic
            }
            Finding::MissingCases { cases, total } => {
                let mut diagnostic = Self::missing_cases(cases, total, location);
                if self.mode != HitMode::Collect {
                    diagnostic.args.insert("catchAll", "true".to_string());
                }
                diagnostic
            }
            Finding::CompressibleTable { before, rules } => {
                let after = rules.len();
                let rules: Vec<Value> = rules
                    .into_iter()
                    .map(|rule| {
                        Value::Object(
                            rule.into_iter()
                                .map(|(key, value)| {
                                    (key.to_string(), Value::String(value.to_string()))
                                })
                                .collect(),
                        )
                    })
                    .collect();
                let mut diagnostic = Diagnostic::hint(
                    DiagnosticCode::CompressibleTable,
                    location(None),
                    "this table can be compressed without changing its results",
                );
                diagnostic.args = DiagnosticArgs::from([
                    ("rowsBefore", before.to_string()),
                    ("rowsAfter", after.to_string()),
                    ("rules", Value::Array(rules).to_string()),
                ]);
                diagnostic
            }
            Finding::ChecksIncomplete {
                rows,
                coverage,
                gaps,
            } => {
                let checks = match (coverage, gaps) {
                    (true, true) => "rows that never fire and missing cases",
                    (true, false) => "rows that never fire",
                    _ => "missing cases",
                };
                let message =
                    format!("this table ({rows} rows) is too complex to check {checks} completely");
                let mut diagnostic = Diagnostic::hint(
                    DiagnosticCode::TableChecksIncomplete,
                    location(None),
                    message,
                );
                diagnostic.args = DiagnosticArgs::from([
                    ("rows", rows.to_string()),
                    ("coverage", coverage.to_string()),
                    ("gaps", gaps.to_string()),
                ]);
                diagnostic
            }
            Finding::OutputNeverProduced { col, values } => {
                let label = self
                    .outputs
                    .iter()
                    .find(|output| output.id == col)
                    .map(|output| output.label.clone())
                    .unwrap_or_else(|| col.clone());
                let value = values
                    .iter()
                    .map(|v| format!("\"{v}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut diagnostic = Diagnostic::hint(
                    DiagnosticCode::OutputNeverProduced,
                    location(Some(col.clone()))
                        .with_target(CursorTarget::DecisionTableHead { col: col.clone() }),
                    format!("no reachable row produces {value} for {label}"),
                );
                diagnostic.args = DiagnosticArgs::from([
                    ("field", label.to_string()),
                    ("value", value),
                    ("count", values.len().to_string()),
                ]);
                diagnostic
            }
            Finding::CellCoversDomain { row, col } => {
                let label = self.label_of(&col);
                let mut args = row_args(row);
                args.insert("col", col.to_string());
                let mut diagnostic = Diagnostic::hint(
                    DiagnosticCode::CellCoversDomain,
                    location(Some(col.clone())).with_target(CursorTarget::DecisionTableCell {
                        row: row_key(row),
                        col,
                    }),
                    format!(
                        "this condition accepts every possible {label} value, so the cell can be empty"
                    ),
                );
                diagnostic.args = args;
                diagnostic
            }
            Finding::DuplicateRule { row, of, redundant } => {
                let mut args = row_args(row);
                args.insert("duplicateOf", (of + 1).to_string());
                args.insert("duplicateOfId", row_key(of).to_string());
                let severity = match redundant {
                    true => Diagnostic::hint,
                    false => Diagnostic::warning,
                };
                let mut diagnostic = severity(
                    DiagnosticCode::DuplicateRule,
                    location(None).with_target(row_target(row)),
                    format!("row {} duplicates row {}", row + 1, of + 1),
                );
                if redundant {
                    args.insert("redundant", "true".to_string());
                }
                diagnostic.args = args;
                diagnostic
            }
        }
    }

    fn missing_cases(
        cases: Vec<GapCase>,
        total: usize,
        location: &impl Fn(Option<Arc<str>>) -> DiagnosticLocation,
    ) -> Diagnostic {
        let describe = |case: &GapCase| {
            if case.parts.is_empty() {
                return "any input".to_string();
            }
            case.parts
                .iter()
                .map(|(label, text)| format!("{label} {}", CellText::brief(text)))
                .collect::<Vec<_>>()
                .join(" and ")
        };
        let descriptions: Vec<String> = cases.iter().map(describe).collect();
        let shown: Vec<String> = descriptions.iter().take(5).cloned().collect();
        let more = total.saturating_sub(shown.len());
        let noun = if total == 1 { "case" } else { "cases" };
        let mut message = format!("no row matches {total} input {noun}: {}", shown.join("; "));
        if more > 0 {
            message.push_str(&format!("; and {more} more"));
        }
        let json_cases: Vec<Value> = cases
            .iter()
            .zip(&descriptions)
            .map(|(case, description)| {
                let mut entry = Map::new();
                entry.insert("description".into(), Value::String(description.clone()));
                if let Some(cells) = &case.cells {
                    let cells: Map<String, Value> = cells
                        .iter()
                        .map(|(col, text)| (col.to_string(), Value::String(text.clone())))
                        .collect();
                    entry.insert("cells".into(), Value::Object(cells));
                }
                if let Some(example) = &case.example {
                    entry.insert("example".into(), example.clone());
                }
                Value::Object(entry)
            })
            .collect();
        let mut diagnostic =
            Diagnostic::hint(DiagnosticCode::MissingCases, location(None), message);
        diagnostic.args = DiagnosticArgs::from([
            ("count", total.to_string()),
            ("shown", shown.join("; ")),
            ("more", more.to_string()),
            ("cases", Value::Array(json_cases).to_string()),
        ]);
        diagnostic
    }

    fn label_of(&self, col: &Arc<str>) -> Arc<str> {
        self.inputs
            .iter()
            .find(|input| input.id == *col)
            .map(|input| input.label.clone())
            .unwrap_or_else(|| col.clone())
    }

    fn row_list(rows: &[usize]) -> String {
        rows.iter()
            .map(|r| (r + 1).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

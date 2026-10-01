use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::model::DecisionContent;

use zen_engine::policy::{
    CursorTarget, DiagnosticCode, EvaluateRequest, PolicyWorkspace, Severity, Workspace,
};
use zen_engine::Decision;

#[derive(Debug)]
struct Gaps {
    severity: Severity,
    message: String,
    cases: Value,
}

struct Table<'a> {
    hit: &'a str,
    inputs: &'a [&'a str],
    outputs: &'a [&'a str],
    rows: &'a [(&'a str, &'a [&'a str], &'a [&'a str])],
}

impl Table<'_> {
    fn content(&self) -> Value {
        let inputs: Vec<Value> = self
            .inputs
            .iter()
            .enumerate()
            .map(|(i, field)| match *field {
                "" => json!({ "id": format!("i{i}"), "name": format!("In {i}") }),
                field => {
                    json!({ "id": format!("i{i}"), "name": format!("In {i}"), "field": field })
                }
            })
            .collect();
        let outputs: Vec<Value> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, field)| json!({ "id": format!("o{i}"), "name": format!("Out {i}"), "field": field }))
            .collect();
        let rules: Vec<Value> = self
            .rows
            .iter()
            .map(|(id, cells, outs)| {
                let mut rule = serde_json::Map::new();
                if !id.is_empty() {
                    rule.insert("_id".into(), json!(id));
                }
                for (i, cell) in cells.iter().enumerate() {
                    rule.insert(format!("i{i}"), json!(cell));
                }
                for (i, cell) in outs.iter().enumerate() {
                    rule.insert(format!("o{i}"), json!(cell));
                }
                Value::Object(rule)
            })
            .collect();
        json!({ "hitPolicy": self.hit, "inputs": inputs, "outputs": outputs, "rules": rules })
    }

    fn policy_diagnostics(&self) -> Vec<zen_engine::policy::Diagnostic> {
        let mut ws = PolicyWorkspace::new();
        ws.set_policy(
            "p",
            serde_json::from_value(self.policy_doc()).expect("policy"),
        );
        ws.diagnostics("p")
    }

    fn policy_findings(&self) -> Vec<String> {
        Self::findings(self.policy_diagnostics())
    }

    fn policy_doc(&self) -> Value {
        json!({ "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "applicant",
                "properties": [
                    { "id": "p1", "name": "tier", "type": "string", "enum": ["gold", "silver", "bronze"], "array": false, "optional": false },
                    { "id": "p2", "name": "age", "type": "number", "array": false, "optional": false },
                    { "id": "p3", "name": "scores", "type": "number", "array": true, "optional": false },
                    { "id": "p4", "name": "vip", "type": "boolean", "array": false, "optional": false },
                    { "id": "p5", "name": "code", "type": "string", "array": false, "optional": true },
                    { "id": "p6", "name": "since", "type": "date", "array": false, "optional": true }
                ]
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": self.content() } }
        ] })
    }

    fn graph_diagnostics(&self) -> Vec<zen_engine::policy::Diagnostic> {
        let mut ws = Workspace::new();
        ws.set_document("g", self.graph_content());
        ws.diagnostics("g")
    }

    fn graph_findings(&self) -> Vec<String> {
        Self::findings(self.graph_diagnostics())
    }

    fn gaps(diagnostics: Vec<zen_engine::policy::Diagnostic>) -> Option<Gaps> {
        let mut missing = diagnostics
            .into_iter()
            .filter(|d| d.code == DiagnosticCode::MissingCases);
        let d = missing.next()?;
        assert!(missing.next().is_none());
        assert!(d.location.target.is_none(), "{d:?}");
        Some(Gaps {
            severity: d.severity,
            message: d.message.clone(),
            cases: serde_json::from_str(d.args.get("cases").expect("cases")).expect("json"),
        })
    }

    fn assert_gaps(&self, severity: Severity, message: &str, cases: Value) {
        for (kind, gaps) in [
            ("policy", Self::gaps(self.policy_diagnostics())),
            ("graph", Self::gaps(self.graph_diagnostics())),
        ] {
            let gaps = gaps.unwrap_or_else(|| panic!("{kind}: no MissingCases"));
            assert_eq!(gaps.message, message, "{kind}");
            assert_eq!(gaps.cases, cases, "{kind}");
            assert_eq!(gaps.severity, severity, "{kind}");
        }
    }

    fn coded(
        diagnostics: Vec<zen_engine::policy::Diagnostic>,
        code: DiagnosticCode,
    ) -> Vec<String> {
        let mut out: Vec<String> = diagnostics
            .into_iter()
            .filter(|d| d.code == code)
            .map(|d| {
                assert_eq!(d.severity, Severity::Hint, "{d:?}");
                let mut parts = vec![match &d.location.target {
                    Some(CursorTarget::DecisionTableRow { row }) => row.to_string(),
                    Some(CursorTarget::DecisionTableCell { row, col }) => format!("{row}/{col}"),
                    other => panic!("unexpected target {other:?}"),
                }];
                for key in ["mergeIntoId", "col", "cell"] {
                    if let Some(value) = d.args.get(key) {
                        parts.push(format!("{key}={value}"));
                    }
                }
                parts.join(" ")
            })
            .collect();
        out.sort();
        out
    }

    fn assert_coded(&self, code: DiagnosticCode, expected: &[&str]) {
        let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            Self::coded(self.policy_diagnostics(), code),
            expected,
            "policy"
        );
        assert_eq!(
            Self::coded(self.graph_diagnostics(), code),
            expected,
            "graph"
        );
    }

    fn assert_no_gaps(&self) {
        assert!(Self::gaps(self.policy_diagnostics()).is_none(), "policy");
        assert!(Self::gaps(self.graph_diagnostics()).is_none(), "graph");
    }

    fn graph_content(&self) -> DecisionContent {
        serde_json::from_value(self.graph_json()).expect("graph")
    }

    fn graph_json(&self) -> Value {
        let schema = json!({
            "type": "object",
            "properties": {
                "applicant": {
                    "type": "object",
                    "properties": {
                        "tier": { "type": "string", "enum": ["gold", "silver", "bronze"] },
                        "age": { "type": "number" },
                        "scores": { "type": "array", "items": { "type": "number" } },
                        "vip": { "type": "boolean" },
                        "code": { "type": "string" },
                        "since": { "type": "string", "format": "date" }
                    },
                    "required": ["tier", "age", "scores", "vip"]
                }
            },
            "required": ["applicant"]
        });
        let graph = json!({
            "nodes": [
                { "id": "in", "name": "in", "type": "inputNode", "content": { "schema": schema.to_string() } },
                { "id": "dt", "name": "dt", "type": "decisionTableNode", "content": self.content() },
                { "id": "out", "name": "out", "type": "outputNode", "content": {} }
            ],
            "edges": [
                { "id": "e1", "sourceId": "in", "targetId": "dt", "sourceHandle": null },
                { "id": "e2", "sourceId": "dt", "targetId": "out", "sourceHandle": null }
            ]
        });
        graph
    }

    fn findings(diagnostics: Vec<zen_engine::policy::Diagnostic>) -> Vec<String> {
        let mut out: Vec<String> = diagnostics
            .into_iter()
            .filter(|d| {
                matches!(
                    d.code,
                    DiagnosticCode::UnsatisfiableCell
                        | DiagnosticCode::UnreachableRule
                        | DiagnosticCode::DuplicateRule
                )
            })
            .map(|d| {
                let expected = match d.args.get("redundant").map(String::as_str) {
                    Some("true") => Severity::Hint,
                    _ => Severity::Warning,
                };
                assert_eq!(d.severity, expected, "{d:?}");
                let target = match &d.location.target {
                    Some(CursorTarget::DecisionTableRow { row }) => row.to_string(),
                    Some(CursorTarget::DecisionTableCell { row, col }) => format!("{row}/{col}"),
                    other => panic!("unexpected target {other:?}"),
                };
                let mut parts = vec![format!("{:?}", d.code), target];
                for key in ["coveredByIds", "duplicateOfId", "example"] {
                    if let Some(value) = d.args.get(key) {
                        parts.push(format!("{key}={value}"));
                    }
                }
                parts.join(" ")
            })
            .collect();
        out.sort();
        out
    }

    fn assert_both(&self, expected: &[&str]) {
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(self.policy_findings(), expected, "policy");
        assert_eq!(self.graph_findings(), expected, "graph");
    }
}

const TIER_AGE: &[&str] = &["applicant.tier", "applicant.age"];

const WORKED_EXAMPLE: Table<'static> = Table {
    hit: "first",
    inputs: TIER_AGE,
    outputs: &["applicant.discount"],
    rows: &[
        ("r1", &["\"gold\"", "< 30"], &["0.2"]),
        ("r2", &["\"gold\"", ">= 30"], &["0.15"]),
        ("r3", &["\"gold\"", "[25..35]"], &["0.1"]),
        ("r4", &["\"silver\"", "> 5 and < 3"], &["0.05"]),
        ("r5", &["\"silver\"", ">= 18"], &["0.05"]),
        ("r6", &["\"silver\"", ">= 18"], &["0.05"]),
        ("r7", &["\"bronze\"", "[18..30]"], &["0"]),
        ("r8", &["\"bronze\"", "(30..65]"], &["0"]),
        (
            "r9",
            &["\"gold\", \"silver\", \"bronze\"", "> 70"],
            &["0.3"],
        ),
    ],
};

#[test]
fn first_hit_worked_example() {
    WORKED_EXAMPLE.assert_both(&[
        "DuplicateRule r6 duplicateOfId=r5",
        "UnsatisfiableCell r4/i1",
        "UnreachableRule r3 coveredByIds=r1,r2 example={\"applicant\":{\"age\":25,\"tier\":\"gold\"}}",
    ]);
}

#[test]
fn same_inputs_with_different_outputs_is_unreachable_not_duplicate() {
    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\"", ">= 18"], &["0.1"]),
            ("r2", &["\"gold\"", ">= 18"], &["0.2"]),
        ],
    }
    .assert_both(&[
        "UnreachableRule r2 coveredByIds=r1 example={\"applicant\":{\"age\":18,\"tier\":\"gold\"}}",
    ]);
}

#[test]
fn catch_all_covers_everything_after_it() {
    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[("r1", &["", ""], &["0"]), ("r2", &["\"gold\"", ""], &["1"])],
    }
    .assert_both(&[
        "UnreachableRule r2 coveredByIds=r1 example={\"applicant\":{\"tier\":\"gold\"}}",
    ]);
}

#[test]
fn collect_reports_only_exact_duplicates() {
    Table {
        hit: "collect",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["", ""], &["0"]),
            ("r2", &["\"gold\"", ">= 18"], &["0.1"]),
            ("r3", &["\"gold\"", ">= 18"], &["0.1"]),
            ("r4", &["\"gold\"", ">= 18"], &["0.2"]),
        ],
    }
    .assert_both(&["DuplicateRule r3 duplicateOfId=r2"]);
}

#[test]
fn first_hit_rows_with_collect_cells_still_fire() {
    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount", "applicant.tags[]"],
        rows: &[
            ("r1", &["\"gold\"", ""], &["0.1", ""]),
            ("r2", &["\"gold\"", "> 30"], &["", "\"senior\""]),
            ("r3", &["\"gold\"", "> 40"], &["0.3", ""]),
        ],
    }
    .assert_both(&[
        "UnreachableRule r3 coveredByIds=r1 example={\"applicant\":{\"age\":41,\"tier\":\"gold\"}}",
    ]);
}

#[test]
fn policy_first_hit_is_per_output_column() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount", "applicant.band"],
        rows: &[
            ("r1", &[">= 18"], &["0.1", ""]),
            ("r2", &[">= 30"], &["0.2", "\"senior\""]),
        ],
    };
    assert_eq!(table.policy_findings(), Vec::<String>::new());
    assert_eq!(
        table.graph_findings(),
        vec!["UnreachableRule r2 coveredByIds=r1 example={\"applicant\":{\"age\":30}}".to_string()]
    );
}

#[test]
fn policy_per_column_coverage_cites_each_column_writer() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount", "applicant.band"],
        rows: &[
            ("r1", &[">= 18"], &["0.1", ""]),
            ("r2", &[">= 0"], &["", "\"adult\""]),
            ("r3", &[">= 30"], &["0.2", "\"senior\""]),
        ],
    };
    assert_eq!(
        table.policy_findings(),
        vec![
            "UnreachableRule r3 coveredByIds=r1,r2 example={\"applicant\":{\"age\":30}}"
                .to_string()
        ]
    );
    assert_eq!(
        table.graph_findings(),
        vec!["UnreachableRule r3 coveredByIds=r1 example={\"applicant\":{\"age\":30}}".to_string()]
    );
}

#[test]
fn not_equal_and_not_in_accept_null() {
    Table {
        hit: "first",
        inputs: &["applicant.code"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["!= \"a\""], &["1"]),
            ("r2", &["\"b\""], &["2"]),
            ("r3", &["null"], &["3"]),
        ],
    }
    .assert_both(&[
        "UnreachableRule r2 coveredByIds=r1 example={\"applicant\":{\"code\":\"b\"}}",
        "UnreachableRule r3 coveredByIds=r1 example={\"applicant\":{\"code\":null}}",
    ]);

    Table {
        hit: "first",
        inputs: &["applicant.code"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["not in [\"a\", \"b\"]"], &["1"]),
            ("r2", &["\"a\", \"b\""], &["2"]),
            ("r3", &["null"], &["3"]),
            ("r4", &["\"c\""], &["4"]),
        ],
    }
    .assert_both(&[
        "UnreachableRule r3 coveredByIds=r1 example={\"applicant\":{\"code\":null}}",
        "UnreachableRule r4 coveredByIds=r1 example={\"applicant\":{\"code\":\"c\"}}",
    ]);
}

#[test]
fn comparisons_do_not_cover_null() {
    Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["> 5"], &["1"]),
            ("r2", &["<= 5"], &["2"]),
            ("r3", &["null"], &["3"]),
            ("r4", &["[0..10]"], &["4"]),
        ],
    }
    .assert_both(&["UnreachableRule r4 coveredByIds=r1,r2 example={\"applicant\":{\"age\":0}}"]);
}

#[test]
fn opaque_cells_are_only_proven_through_identical_atoms() {
    Table {
        hit: "first",
        inputs: &["applicant.scores", "applicant.tier"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["some($, # > 3)", "\"gold\""], &["1"]),
            ("r2", &["some($,  # > 3)", "\"gold\""], &["1"]),
            ("r3", &["some($, # > 3)", "\"gold\", \"silver\""], &["2"]),
            ("r4", &["len($) > 3", "\"gold\""], &["3"]),
            ("r5", &["", "\"silver\""], &["4"]),
            ("r6", &["some($, # > 3)", "\"silver\""], &["5"]),
        ],
    }
    .assert_both(&[
        "DuplicateRule r2 duplicateOfId=r1",
        "UnreachableRule r6 coveredByIds=r3",
    ]);
}

#[test]
fn earlier_opaque_cells_never_cover() {
    Table {
        hit: "first",
        inputs: &["applicant.scores", "applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["some($, # > 3)", ""], &["1"]),
            ("r2", &["", "> 18"], &["2"]),
        ],
    }
    .assert_both(&[]);
}

#[test]
fn expression_columns_are_opaque() {
    Table {
        hit: "first",
        inputs: &["", "applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["applicant.vip == true", "> 18"], &["1"]),
            ("r2", &["applicant.vip  ==  true", "> 18"], &["1"]),
            ("r3", &["applicant.vip == false", "> 18"], &["1"]),
            ("r4", &["", "> 18"], &["2"]),
            ("r5", &["applicant.vip == false", "> 20"], &["3"]),
        ],
    }
    .assert_both(&[
        "DuplicateRule r2 duplicateOfId=r1",
        "UnreachableRule r5 coveredByIds=r3",
    ]);
}

#[test]
fn nondeterministic_cells_are_never_equal() {
    Table {
        hit: "first",
        inputs: &["", "applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["rand(10) > 5", ""], &["1"]),
            ("r2", &["rand(10) > 5", ""], &["1"]),
        ],
    }
    .assert_both(&[]);
}

#[test]
fn rows_with_empty_cells_everywhere_are_unaffected() {
    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[("r1", &["", ""], &["1"])],
    }
    .assert_both(&[]);
}

#[test]
fn graph_rows_without_ids_use_the_index() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[("", &["> 5"], &["1"]), ("", &["> 10"], &["2"])],
    };
    assert_eq!(
        table.graph_findings(),
        vec!["UnreachableRule 1 coveredByIds=0 example={\"applicant\":{\"age\":11}}".to_string()]
    );
}

#[test]
fn tables_over_the_row_cap_skip_coverage_but_keep_cell_checks() {
    let rows: Vec<(String, [&str; 1], [String; 1])> = (0..2001)
        .map(|i| {
            (
                format!("r{i}"),
                [if i == 0 { "> 5 and < 3" } else { "> 5" }],
                [i.to_string()],
            )
        })
        .collect();
    let outs: Vec<[&str; 1]> = rows.iter().map(|(_, _, out)| [out[0].as_str()]).collect();
    let rows: Vec<(&str, &[&str], &[&str])> = rows
        .iter()
        .zip(&outs)
        .map(|((id, cells, _), outs)| (id.as_str(), &cells[..], &outs[..]))
        .collect();
    Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &rows,
    }
    .assert_both(&["UnsatisfiableCell r0/i0"]);
}

#[test]
fn bool_and_dictionary_cells() {
    Table {
        hit: "first",
        inputs: &["applicant.vip"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["true"], &["1"]),
            ("r2", &["false"], &["2"]),
            ("r3", &["true, false"], &["3"]),
        ],
    }
    .assert_both(&["UnreachableRule r3 coveredByIds=r1,r2 example={\"applicant\":{\"vip\":true}}"]);
}

#[tokio::test]
async fn reported_example_is_answered_by_the_covering_rows() {
    let example = json!({ "applicant": { "age": 25, "tier": "gold", "scores": [], "vip": false } });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(WORKED_EXAMPLE.policy_doc()).expect("policy"),
    );
    let result = ws
        .evaluate(&EvaluateRequest {
            policy_path: Arc::from("p"),
            input: example.clone().into(),
            goals: Vec::new(),
            trace: false,
        })
        .expect("evaluate");
    let output: Value = result.output.into();
    assert_eq!(output.pointer("/applicant/discount"), Some(&json!(0.2)));

    let DecisionContent::Graph(graph) = WORKED_EXAMPLE.graph_content() else {
        panic!("expected graph content");
    };
    let decision = Decision::from(graph);
    let response = decision.evaluate(example.into()).await.expect("graph");
    let output: Value = response.result.into();
    assert_eq!(output.pointer("/applicant/discount"), Some(&json!(0.2)));
}

#[test]
fn missing_cases_merge_regions_with_examples() {
    WORKED_EXAMPLE.assert_gaps(
        Severity::Hint,
        "no row matches 2 input cases: In 0 \"bronze\" and In 1 < 18, (65..70]; In 0 \"silver\" and In 1 < 18",
        json!([
            {
                "cells": { "i0": "\"bronze\"", "i1": "< 18, (65..70]" },
                "description": "In 0 \"bronze\" and In 1 < 18, (65..70]",
                "example": { "applicant": { "age": 17, "tier": "bronze" } }
            },
            {
                "cells": { "i0": "\"silver\"", "i1": "< 18" },
                "description": "In 0 \"silver\" and In 1 < 18",
                "example": { "applicant": { "age": 17, "tier": "silver" } }
            }
        ]),
    );
}

#[test]
fn covered_tables_have_no_missing_cases() {
    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\", \"silver\"", ""], &["1"]),
            ("r2", &["\"bronze\"", "< 18"], &["2"]),
            ("r3", &["\"bronze\"", ">= 18"], &["3"]),
        ],
    }
    .assert_no_gaps();

    Table {
        hit: "collect",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[("r1", &["", ""], &["1"]), ("r2", &["\"gold\"", ""], &["2"])],
    }
    .assert_no_gaps();
}

#[test]
fn collect_tables_report_inputs_with_empty_results() {
    Table {
        hit: "collect",
        inputs: &["applicant.vip"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["true"], &["1"])],
    }
    .assert_gaps(
        Severity::Hint,
        "no row matches 1 input case: In 0 false",
        json!([{
            "cells": { "i0": "false" },
            "description": "In 0 false",
            "example": { "applicant": { "vip": false } }
        }]),
    );
}

#[test]
fn optional_fields_include_null_in_gaps() {
    Table {
        hit: "first",
        inputs: &["applicant.code"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["\"a\", \"b\""], &["1"])],
    }
    .assert_gaps(
        Severity::Hint,
        "no row matches 1 input case: In 0 not in [\"a\", \"b\"]",
        json!([{
            "cells": { "i0": "not in [\"a\", \"b\"]" },
            "description": "In 0 not in [\"a\", \"b\"]",
            "example": { "applicant": { "code": "other" } }
        }]),
    );

    Table {
        hit: "first",
        inputs: &["applicant.code"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["\"a\""], &["1"]), ("r2", &["!= \"a\""], &["2"])],
    }
    .assert_no_gaps();
}

#[test]
fn opaque_cells_never_create_gaps() {
    Table {
        hit: "first",
        inputs: &["applicant.scores", "applicant.tier"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["some($, # > 3)", ""], &["1"]),
            ("r2", &["", "\"gold\""], &["2"]),
        ],
    }
    .assert_no_gaps();

    Table {
        hit: "first",
        inputs: &["", "applicant.vip"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["applicant.age > 10", "true"], &["1"])],
    }
    .assert_gaps(
        Severity::Hint,
        "no row matches 1 input case: In 1 false",
        json!([{
            "cells": { "i1": "false" },
            "description": "In 1 false",
            "example": { "applicant": { "vip": false } }
        }]),
    );
}

#[test]
fn columns_on_the_same_field_are_one_dimension() {
    Table {
        hit: "first",
        inputs: &["applicant.age", "applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &[">= 18", ""], &["1"]),
            ("r2", &["", "< 18"], &["2"]),
        ],
    }
    .assert_no_gaps();
}

#[test]
fn gaps_stay_hints_when_the_output_is_read_downstream() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.vip"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["true"], &["1"])],
    };

    let mut doc = table.policy_doc();
    doc["blocks"].as_array_mut().expect("blocks").push(json!({
        "id": "calc",
        "type": "expression",
        "props": { "data": { "key": "applicant.total", "value": "applicant.discount * 2" } }
    }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).expect("policy"));
    let gaps = Table::gaps(ws.diagnostics("p")).expect("policy gaps");
    assert_eq!(gaps.severity, Severity::Hint);

    let mut graph = table.graph_json();
    let nodes = graph["nodes"].as_array_mut().expect("nodes");
    nodes.push(json!({
        "id": "calc", "name": "calc", "type": "expressionNode",
        "content": { "expressions": [ { "id": "x1", "key": "total", "value": "applicant.discount * 2" } ] }
    }));
    graph["edges"] = json!([
        { "id": "e1", "sourceId": "in", "targetId": "dt", "sourceHandle": null },
        { "id": "e2", "sourceId": "dt", "targetId": "calc", "sourceHandle": null },
        { "id": "e3", "sourceId": "calc", "targetId": "out", "sourceHandle": null }
    ]);
    let mut ws = Workspace::new();
    ws.set_document("g", serde_json::from_value(graph).expect("graph"));
    let gaps = Table::gaps(ws.diagnostics("g")).expect("graph gaps");
    assert_eq!(gaps.severity, Severity::Hint);
}

#[test]
fn graph_schema_ranges_narrow_number_domains() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["[0..18)"], &["1"]), ("r2", &["[18..120]"], &["2"])],
    };
    assert!(Table::gaps(table.policy_diagnostics()).is_some());

    let mut graph = table.graph_json();
    let input = graph["nodes"]
        .as_array_mut()
        .expect("nodes")
        .iter_mut()
        .find(|n| n["id"] == "in")
        .expect("input node");
    let mut schema: Value =
        serde_json::from_str(input["content"]["schema"].as_str().expect("schema")).expect("json");
    schema["properties"]["applicant"]["properties"]["age"] =
        json!({ "type": "number", "minimum": 0, "maximum": 120 });
    input["content"]["schema"] = Value::String(schema.to_string());
    let mut ws = Workspace::new();
    ws.set_document("g", serde_json::from_value(graph).expect("graph"));
    assert!(Table::gaps(ws.diagnostics("g")).is_none());
}

fn compressed(diagnostics: Vec<zen_engine::policy::Diagnostic>) -> Option<(usize, Value)> {
    let d = diagnostics
        .into_iter()
        .find(|d| d.code == DiagnosticCode::CompressibleTable)?;
    assert_eq!(d.severity, Severity::Hint);
    assert!(d.location.target.is_none());
    let rules: Value = serde_json::from_str(d.args.get("rules").expect("rules")).expect("json");
    let before: usize = d
        .args
        .get("rowsBefore")
        .expect("before")
        .parse()
        .expect("number");
    assert_eq!(
        d.args.get("rowsAfter").map(String::as_str),
        Some(rules.as_array().expect("array").len().to_string().as_str())
    );
    Some((before, rules))
}

fn row_summary(rules: &Value) -> Vec<String> {
    rules
        .as_array()
        .expect("rules")
        .iter()
        .map(|rule| {
            let mut keys: Vec<&String> = rule.as_object().expect("rule").keys().collect();
            keys.sort();
            keys.iter()
                .filter(|k| k.as_str() != "_id")
                .map(|k| format!("{k}={}", rule[k.as_str()].as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

impl Table<'_> {
    fn assert_compressed(&self, expected: Option<(usize, &[&str])>) {
        for (kind, found) in [
            ("policy", compressed(self.policy_diagnostics())),
            ("graph", compressed(self.graph_diagnostics())),
        ] {
            let found = found.map(|(before, rules)| (before, row_summary(&rules)));
            let expected = expected.map(|(before, rows)| {
                (
                    before,
                    rows.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
                )
            });
            assert_eq!(found, expected, "{kind}");
        }
    }
}

#[test]
fn compress_merges_adjacent_ranges_and_drops_duplicates() {
    WORKED_EXAMPLE.assert_compressed(Some((
        9,
        &[
            "i0=\"gold\" i1=< 30 o0=0.2",
            "i0=\"gold\" i1=>= 30 o0=0.15",
            "i0=\"gold\" i1=[25..35] o0=0.1",
            "i0=\"silver\" i1=> 5 and < 3 o0=0.05",
            "i0=\"silver\" i1=>= 18 o0=0.05",
            "i0=\"bronze\" i1=[18..65] o0=0",
            "i0=\"gold\", \"silver\", \"bronze\" i1=> 70 o0=0.3",
        ],
    )));
}

#[test]
fn compress_merges_value_lists() {
    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\"", "< 30"], &["0.1"]),
            ("r2", &["\"silver\"", "< 30"], &["0.1"]),
            ("r3", &["\"bronze\"", "< 18"], &["0.2"]),
            ("r4", &["\"bronze\"", "> 65"], &["0.2"]),
        ],
    }
    .assert_compressed(Some((
        4,
        &[
            "i0=\"gold\", \"silver\" i1=< 30 o0=0.1",
            "i0=\"bronze\" i1=< 18, > 65 o0=0.2",
        ],
    )));
}

#[test]
fn compress_absorbs_subsumed_rows() {
    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\"", "< 30"], &["0.1"]),
            ("r2", &["\"gold\"", ""], &["0.1"]),
            ("r3", &["", ""], &["0"]),
        ],
    }
    .assert_compressed(Some((3, &["i0=\"gold\" i1= o0=0.1", "i0= i1= o0=0"])));

    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\"", "< 30"], &["0.1"]),
            ("r2", &["", "< 18"], &["0.5"]),
            ("r3", &["\"gold\"", ""], &["0.1"]),
        ],
    }
    .assert_compressed(Some((3, &["i0=\"gold\" i1= o0=0.1", "i0= i1=< 18 o0=0.5"])));

    Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\"", "< 30"], &["0.1"]),
            ("r2", &["", "[20..40]"], &["0.5"]),
            ("r3", &["\"gold\"", ""], &["0.1"]),
        ],
    }
    .assert_compressed(None);
}

#[test]
fn compress_respects_order_and_collect() {
    Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["< 18"], &["1"]),
            ("r2", &["< 30"], &["2"]),
            ("r3", &["[18..30)"], &["1"]),
        ],
    }
    .assert_compressed(None);

    Table {
        hit: "collect",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["< 30"], &["1"]), ("r2", &["[18..40]"], &["1"])],
    }
    .assert_compressed(None);

    Table {
        hit: "collect",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["< 18"], &["1"]), ("r2", &["[18..40]"], &["1"])],
    }
    .assert_compressed(Some((2, &["i0=<= 40 o0=1"])));

    Table {
        hit: "collect",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &[">= 18"], &["1"]), ("r2", &[">= 18"], &["1"])],
    }
    .assert_compressed(None);
}

#[test]
fn cells_covering_the_whole_domain() {
    Table {
        hit: "first",
        inputs: &["applicant.tier", "applicant.vip", "applicant.code"],
        outputs: &["applicant.discount"],
        rows: &[
            (
                "r1",
                &[
                    "\"gold\", \"silver\", \"bronze\"",
                    "true, false",
                    "\"a\", \"b\"",
                ],
                &["1"],
            ),
            ("r2", &["\"gold\"", "", "!= null"], &["2"]),
        ],
    }
    .assert_coded(
        DiagnosticCode::CellCoversDomain,
        &["r1/i0 col=i0", "r1/i1 col=i1"],
    );
}

fn switch_graph(table: &Table, condition: &str, handle: &str, middle: Option<Value>) -> Value {
    let mut graph = table.graph_json();
    let nodes = graph["nodes"].as_array_mut().expect("nodes");
    nodes.push(json!({
        "id": "sw", "name": "sw", "type": "switchNode",
        "content": { "hitPolicy": "first", "statements": [
            { "id": "s1", "condition": condition },
            { "id": "s2", "condition": "" }
        ] }
    }));
    let target = match middle {
        Some(node) => {
            nodes.push(node);
            "mid"
        }
        None => "dt",
    };
    let other = if handle == "s1" { "s2" } else { "s1" };
    let mut edges = vec![
        json!({ "id": "e1", "sourceId": "in", "targetId": "sw", "sourceHandle": null }),
        json!({ "id": "e2", "sourceId": "sw", "targetId": target, "sourceHandle": handle }),
        json!({ "id": "e3", "sourceId": "sw", "targetId": "out", "sourceHandle": other }),
        json!({ "id": "e4", "sourceId": "dt", "targetId": "out", "sourceHandle": null }),
    ];
    if target == "mid" {
        edges
            .push(json!({ "id": "e5", "sourceId": "mid", "targetId": "dt", "sourceHandle": null }));
    }
    graph["edges"] = Value::Array(edges);
    graph
}

fn graph_gaps(graph: Value) -> Option<Gaps> {
    let mut ws = Workspace::new();
    ws.set_document("g", serde_json::from_value(graph).expect("graph"));
    Table::gaps(ws.diagnostics("g"))
}

#[test]
fn switch_branches_narrow_downstream_tables() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["[18..65]"], &["1"]), ("r2", &["> 65"], &["2"])],
    };
    assert!(Table::gaps(table.policy_diagnostics()).is_some());
    assert!(graph_gaps(table.graph_json()).is_some());

    assert!(graph_gaps(switch_graph(&table, "applicant.age >= 18", "s1", None)).is_none());
    assert!(graph_gaps(switch_graph(&table, "applicant.age < 18", "s2", None)).is_none());
    assert!(graph_gaps(switch_graph(
        &table,
        "applicant.age >= 18 and applicant.vip",
        "s1",
        None
    ))
    .is_none());
    assert!(graph_gaps(switch_graph(&table, "applicant.age >= 21", "s1", None)).is_none());
    assert!(graph_gaps(switch_graph(&table, "applicant.age >= 10", "s1", None)).is_some());

    let keeps = json!({
        "id": "mid", "name": "mid", "type": "expressionNode",
        "content": { "expressions": [ { "id": "x1", "key": "applicant.flag", "value": "true" } ], "passThrough": true }
    });
    assert!(graph_gaps(switch_graph(
        &table,
        "applicant.age >= 18",
        "s1",
        Some(keeps)
    ))
    .is_none());

    let rewrites = json!({
        "id": "mid", "name": "mid", "type": "expressionNode",
        "content": { "expressions": [ { "id": "x1", "key": "applicant.age", "value": "applicant.age - 20" } ], "passThrough": true }
    });
    assert!(graph_gaps(switch_graph(
        &table,
        "applicant.age >= 18",
        "s1",
        Some(rewrites)
    ))
    .is_some());
}

#[test]
fn date_columns_compare_by_day() {
    Table {
        hit: "first",
        inputs: &["applicant.since"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["< \"2024-01-01\""], &["1"]),
            ("r2", &[">= \"2024-01-01\""], &["2"]),
            ("r3", &["> \"2025-06-01\""], &["3"]),
            ("r4", &["\"2023-05-05\""], &["4"]),
        ],
    }
    .assert_both(&[
        "UnreachableRule r3 coveredByIds=r2 example={\"applicant\":{\"since\":\"2025-06-02\"}}",
        "UnreachableRule r4 coveredByIds=r1 example={\"applicant\":{\"since\":\"2023-05-05\"}}",
    ]);

    Table {
        hit: "first",
        inputs: &["applicant.since"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["< \"2024-01-01\""], &["1"])],
    }
    .assert_gaps(
        Severity::Hint,
        "no row matches 1 input case: In 0 >= \"2024-01-01\", null",
        json!([{
            "cells": { "i0": ">= \"2024-01-01\", null" },
            "description": "In 0 >= \"2024-01-01\", null",
            "example": { "applicant": { "since": "2024-01-01" } }
        }]),
    );
}

#[test]
fn date_cells_outside_the_model_stay_opaque() {
    Table {
        hit: "first",
        inputs: &["applicant.since"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["!= \"2024-06-01T10:00:00Z\""], &["1"]),
            ("r2", &[">= \"2024-01-01\""], &["2"]),
            ("r3", &["> 5"], &["3"]),
            ("r4", &["> d(\"2024-01-01\")"], &["4"]),
            ("r5", &["[\"2024-01-01\"..\"2024-12-31\"]"], &["5"]),
        ],
    }
    .assert_both(&[]);
}

#[tokio::test]
async fn date_findings_agree_with_the_runtime() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.since"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["< \"2024-01-01\""], &["1"]),
            ("r2", &[">= \"2024-01-01\""], &["2"]),
            ("r3", &["> \"2025-06-01\""], &["3"]),
        ],
    };
    let example = json!({ "applicant": { "since": "2025-06-02", "tier": "gold", "age": 1, "scores": [], "vip": false } });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(table.policy_doc()).expect("policy"),
    );
    let result = ws
        .evaluate(&EvaluateRequest {
            policy_path: Arc::from("p"),
            input: example.clone().into(),
            goals: Vec::new(),
            trace: false,
        })
        .expect("evaluate");
    let output: Value = result.output.into();
    assert_eq!(output.pointer("/applicant/discount"), Some(&json!(2)));

    let DecisionContent::Graph(graph) = table.graph_content() else {
        panic!("expected graph content");
    };
    let response = Decision::from(graph)
        .evaluate(example.into())
        .await
        .expect("graph");
    let output: Value = response.result.into();
    assert_eq!(output.pointer("/applicant/discount"), Some(&json!(2)));
}

fn band_dictionary() -> Value {
    json!({ "id": "dict", "type": "dictionary", "props": { "data": {
        "name": "band",
        "entries": [
            { "id": "e1", "value": "low", "label": "Low" },
            { "id": "e2", "value": "mid", "label": "Mid" },
            { "id": "e3", "value": "high", "label": "High" }
        ]
    } } })
}

fn never_produced(rows: Value) -> (Vec<String>, Vec<String>) {
    let content = json!({
        "hitPolicy": "first",
        "inputs": [ { "id": "i0", "name": "Age", "field": "applicant.age" } ],
        "outputs": [ { "id": "o0", "name": "Band", "field": "applicant.band", "type": "band" } ],
        "rules": rows
    });
    let collect = |diagnostics: Vec<zen_engine::policy::Diagnostic>| -> Vec<String> {
        diagnostics
            .into_iter()
            .filter(|d| d.code == DiagnosticCode::OutputNeverProduced)
            .map(|d| {
                assert_eq!(d.severity, Severity::Hint);
                assert!(matches!(
                    &d.location.target,
                    Some(CursorTarget::DecisionTableHead { col }) if col.as_ref() == "o0"
                ));
                d.message.clone()
            })
            .collect()
    };

    let mut policy_doc = WORKED_EXAMPLE.policy_doc();
    let blocks = policy_doc["blocks"].as_array_mut().expect("blocks");
    blocks.retain(|b| b["id"] != "dt");
    blocks.push(band_dictionary());
    blocks
        .push(json!({ "id": "dt", "type": "decisionTable", "props": { "data": content.clone() } }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(policy_doc).expect("policy"));
    let policy = collect(ws.diagnostics("p"));

    let mut graph = WORKED_EXAMPLE.graph_json();
    graph["imports"] = json!(["dicts"]);
    for node in graph["nodes"].as_array_mut().expect("nodes") {
        if node["id"] == "dt" {
            node["content"] = content.clone();
        }
    }
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        serde_json::from_value(json!({ "blocks": [band_dictionary()] })).expect("dicts"),
    );
    ws.set_document("g", serde_json::from_value(graph).expect("graph"));
    let graph = collect(ws.diagnostics("g"));
    (policy, graph)
}

#[test]
fn dictionary_outputs_never_produced() {
    let (policy, graph) = never_produced(json!([
        { "_id": "r1", "i0": "< 18", "o0": "\"low\"" },
        { "_id": "r2", "i0": "", "o0": "\"mid\"" },
        { "_id": "r3", "i0": "> 65", "o0": "\"high\"" }
    ]));
    let expected = vec!["no reachable row produces \"high\" for applicant.band".to_string()];
    assert_eq!(policy, expected);
    assert_eq!(graph, expected);

    let (policy, graph) = never_produced(json!([
        { "_id": "r1", "i0": "< 18", "o0": "\"low\"" },
        { "_id": "r2", "i0": "", "o0": "applicant.tier == \"gold\" ? \"high\" : \"mid\"" }
    ]));
    assert!(policy.is_empty(), "{policy:?}");
    assert!(graph.is_empty(), "{graph:?}");

    let (policy, graph) = never_produced(json!([
        { "_id": "r1", "i0": "< 18", "o0": "\"low\"" },
        { "_id": "r2", "i0": ">= 18", "o0": "\"mid\"" },
        { "_id": "r3", "i0": "> 65", "o0": "\"high\"" }
    ]));
    assert_eq!(policy, expected);
    assert_eq!(graph, expected);
}

#[test]
fn computed_fields_get_no_example() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.level"],
        outputs: &["applicant.discount"],
        rows: &[("r1", &["> 5"], &["1"]), ("r2", &["> 10"], &["2"])],
    };

    let mut doc = table.policy_doc();
    let blocks = doc["blocks"].as_array_mut().expect("blocks");
    blocks.insert(
        1,
        json!({ "id": "calc", "type": "expression", "props": { "data": { "key": "applicant.level", "value": "applicant.age * 2" } } }),
    );
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).expect("policy"));
    assert_eq!(
        Table::findings(ws.diagnostics("p")),
        vec!["UnreachableRule r2 coveredByIds=r1".to_string()]
    );

    let mut graph = table.graph_json();
    graph["nodes"].as_array_mut().expect("nodes").push(json!({
        "id": "calc", "name": "calc", "type": "expressionNode",
        "content": { "expressions": [ { "id": "x1", "key": "applicant.level", "value": "applicant.age * 2" } ], "passThrough": true }
    }));
    graph["edges"] = json!([
        { "id": "e1", "sourceId": "in", "targetId": "calc", "sourceHandle": null },
        { "id": "e2", "sourceId": "calc", "targetId": "dt", "sourceHandle": null },
        { "id": "e3", "sourceId": "dt", "targetId": "out", "sourceHandle": null }
    ]);
    let mut ws = Workspace::new();
    ws.set_document("g", serde_json::from_value(graph).expect("graph"));
    assert_eq!(
        Table::findings(ws.diagnostics("g")),
        vec!["UnreachableRule r2 coveredByIds=r1".to_string()]
    );
}

async fn outputs_for(content: Value, inputs: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let mut doc = WORKED_EXAMPLE.policy_doc();
    for block in doc["blocks"].as_array_mut().expect("blocks") {
        if block["id"] == "dt" {
            block["props"]["data"] = content.clone();
        }
    }
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).expect("policy"));
    let mut policy = Vec::new();
    for input in inputs {
        let result = ws
            .evaluate(&EvaluateRequest {
                policy_path: Arc::from("p"),
                input: input.clone().into(),
                goals: Vec::new(),
                trace: false,
            })
            .expect("evaluate");
        let output: Value = result.output.into();
        policy.push(
            output
                .pointer("/applicant/discount")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }

    let mut graph_json = WORKED_EXAMPLE.graph_json();
    for node in graph_json["nodes"].as_array_mut().expect("nodes") {
        if node["id"] == "dt" {
            node["content"] = content.clone();
        }
    }
    let DecisionContent::Graph(graph) = serde_json::from_value(graph_json).expect("graph") else {
        panic!("graph");
    };
    let decision = Decision::from(graph);
    let mut graph = Vec::new();
    for input in inputs {
        let response = decision
            .evaluate(input.clone().into())
            .await
            .expect("graph");
        let output: Value = response.result.into();
        graph.push(
            output
                .pointer("/applicant/discount")
                .cloned()
                .unwrap_or(Value::Null),
        );
    }
    (policy, graph)
}

#[tokio::test]
async fn compression_preserves_results() {
    for table in [
        WORKED_EXAMPLE,
        Table {
            hit: "first",
            inputs: TIER_AGE,
            outputs: &["applicant.discount"],
            rows: &[
                ("r1", &["\"gold\"", "< 30"], &["0.1"]),
                ("r2", &["", "< 18"], &["0.5"]),
                ("r3", &["\"gold\"", ""], &["0.1"]),
                ("r4", &["\"silver\"", "< 18"], &["0.2"]),
                ("r5", &["\"bronze\"", "< 18"], &["0.2"]),
            ],
        },
    ] {
        let original = table.content();
        let (_, rules) = compressed(table.policy_diagnostics()).expect("compressible");
        let mut compact = original.clone();
        compact["rules"] = rules;
        let inputs: Vec<Value> = ["gold", "silver", "bronze"]
            .iter()
            .flat_map(|tier| {
                [-1, 0, 17, 18, 24, 25, 29, 30, 31, 35, 36, 64, 65, 66, 70, 71, 100]
                    .iter()
                    .map(move |age| {
                        json!({ "applicant": { "tier": tier, "age": age, "scores": [], "vip": false } })
                    })
            })
            .collect();
        let before = outputs_for(original, &inputs).await;
        let after = outputs_for(compact, &inputs).await;
        assert_eq!(before.0, after.0, "policy");
        assert_eq!(before.1, after.1, "graph");
    }
}

#[test]
fn missing_cases_carry_every_case() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.tier", "applicant.vip", "applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\"", "true", "!= 1"], &["1"]),
            ("r2", &["\"gold\"", "false", "!= 2"], &["1"]),
            ("r3", &["\"silver\"", "true", "!= 3"], &["1"]),
            ("r4", &["\"silver\"", "false", "!= 4"], &["1"]),
            ("r5", &["\"bronze\"", "true", "!= 5"], &["1"]),
            ("r6", &["\"bronze\"", "false", "!= 6"], &["1"]),
        ],
    };
    for diagnostics in [table.policy_diagnostics(), table.graph_diagnostics()] {
        let d = diagnostics
            .into_iter()
            .find(|d| d.code == DiagnosticCode::MissingCases)
            .expect("gaps");
        let cases: Value = serde_json::from_str(d.args.get("cases").expect("cases")).expect("json");
        let count: usize = d.args.get("count").expect("count").parse().expect("number");
        assert_eq!(count, 6);
        assert_eq!(cases.as_array().expect("array").len(), 6);
        assert!(cases
            .as_array()
            .expect("array")
            .iter()
            .all(|c| c["cells"].is_object()));
        assert_eq!(d.args.get("more").map(String::as_str), Some("1"));
    }
}

fn leak_rows(
    rows: Vec<(String, Vec<String>, Vec<String>)>,
) -> &'static [(
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
)] {
    let leak = |values: Vec<String>| -> &'static [&'static str] {
        Box::leak(
            values
                .into_iter()
                .map(|v| &*Box::leak(v.into_boxed_str()))
                .collect::<Vec<&'static str>>()
                .into_boxed_slice(),
        )
    };
    Box::leak(
        rows.into_iter()
            .map(|(id, cells, outs)| (&*Box::leak(id.into_boxed_str()), leak(cells), leak(outs)))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

#[tokio::test]
async fn randomized_compression_preserves_results() {
    let tiers = [
        "",
        "\"gold\"",
        "\"silver\"",
        "\"bronze\"",
        "\"gold\", \"silver\"",
        "\"silver\", \"bronze\"",
    ];
    let ages = [
        "", "< 18", ">= 18", "[18..30)", "< 30", ">= 30", "> 65", "[30..65]", "25",
    ];
    let discounts = ["0.1", "0.2", "0.3"];
    let mut seed: u64 = 0x5eed;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    let inputs: Vec<Value> = ["gold", "silver", "bronze"]
        .iter()
        .flat_map(|tier| {
            [-1, 0, 17, 18, 24, 25, 26, 29, 30, 31, 64, 65, 66, 100]
                .iter()
                .map(move |age| {
                    json!({ "applicant": { "tier": tier, "age": age, "scores": [], "vip": false } })
                })
        })
        .collect();
    let mut checked = 0;
    for _ in 0..80 {
        let count = 3 + next(12);
        let rows: Vec<(String, Vec<String>, Vec<String>)> = (0..count)
            .map(|i| {
                (
                    format!("r{i}"),
                    vec![
                        tiers[next(tiers.len())].to_string(),
                        ages[next(ages.len())].to_string(),
                    ],
                    vec![discounts[next(discounts.len())].to_string()],
                )
            })
            .collect();
        let table = Table {
            hit: "first",
            inputs: TIER_AGE,
            outputs: &["applicant.discount"],
            rows: leak_rows(rows),
        };
        let original = table.content();
        for diagnostics in [table.policy_diagnostics(), table.graph_diagnostics()] {
            let Some((_, rules)) = compressed(diagnostics) else {
                continue;
            };
            let mut compact = original.clone();
            compact["rules"] = rules;
            let before = outputs_for(original.clone(), &inputs).await;
            let after = outputs_for(compact.clone(), &inputs).await;
            assert_eq!(before.0, after.0, "policy {original} -> {compact}");
            assert_eq!(before.1, after.1, "graph {original} -> {compact}");
            checked += 1;
        }
    }
    assert!(checked >= 20, "only {checked} tables compressed");
}

#[test]
fn large_tables_defer_coverage_to_the_full_check() {
    let rows: Vec<(String, Vec<String>, Vec<String>)> = (0..2_010)
        .map(|i| {
            (
                format!("r{i}"),
                vec!["\"gold\"".to_string(), i.to_string()],
                vec![format!("{}", i % 7)],
            )
        })
        .collect();
    let table = Table {
        hit: "first",
        inputs: TIER_AGE,
        outputs: &["applicant.discount"],
        rows: leak_rows(rows),
    };
    let codes = |diagnostics: &[zen_engine::policy::Diagnostic]| -> Vec<DiagnosticCode> {
        diagnostics.iter().map(|d| d.code).collect()
    };

    let mut policy = PolicyWorkspace::new();
    policy.set_policy(
        "p",
        serde_json::from_value(table.policy_doc()).expect("policy"),
    );
    let mut graph = Workspace::new();
    graph.set_document("g", table.graph_content());

    for (ws, path) in [(&policy, "p"), (&graph, "g")] {
        let live: Vec<_> = ws
            .diagnostics(path)
            .into_iter()
            .filter(|d| d.location.block_id.as_deref() == Some("dt"))
            .collect();
        let incomplete = live
            .iter()
            .find(|d| d.code == DiagnosticCode::TableChecksIncomplete)
            .unwrap_or_else(|| panic!("{path}: {:?}", codes(&live)));
        assert_eq!(
            incomplete.args.get("rows").map(String::as_str),
            Some("2010")
        );
        assert_eq!(
            incomplete.args.get("full").map(String::as_str),
            Some("false")
        );
        assert!(
            !codes(&live).contains(&DiagnosticCode::MissingCases),
            "{path}"
        );

        let full = ws.full_table_check(path, "dt");
        assert!(
            codes(&full).contains(&DiagnosticCode::MissingCases),
            "{path}: {:?}",
            codes(&full)
        );
        assert!(
            !codes(&full).contains(&DiagnosticCode::TableChecksIncomplete),
            "{path}: {:?}",
            full.iter().map(|d| d.message.clone()).collect::<Vec<_>>()
        );
        assert!(ws
            .diagnostics(path)
            .iter()
            .any(|d| d.code == DiagnosticCode::TableChecksIncomplete));
    }
}

#[test]
fn identical_conditions_with_different_results_ask_which_to_keep() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.tier"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["\"gold\""], &["0.1"]),
            ("r2", &["\"silver\""], &["0.11"]),
            ("r3", &["\"silver\""], &["0.05"]),
        ],
    };
    for diagnostics in [table.policy_diagnostics(), table.graph_diagnostics()] {
        let d = diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::UnreachableRule)
            .expect("conflict");
        assert_eq!(
            d.args.get("sameConditions").map(String::as_str),
            Some("true")
        );
        assert_eq!(d.args.get("rowId").map(String::as_str), Some("r3"));
        assert_eq!(d.args.get("coveredByIds").map(String::as_str), Some("r2"));
        assert!(
            d.message.contains("same conditions as row 2"),
            "{}",
            d.message
        );
    }
}

#[test]
fn covered_rows_with_the_same_result_are_redundant_hints() {
    let table = Table {
        hit: "first",
        inputs: &["applicant.age"],
        outputs: &["applicant.discount"],
        rows: &[
            ("r1", &["> 20"], &["1"]),
            ("r2", &["> 40"], &["1"]),
            ("r3", &["<= 20"], &["3"]),
        ],
    };
    for diagnostics in [table.policy_diagnostics(), table.graph_diagnostics()] {
        let d = diagnostics
            .iter()
            .find(|d| d.code == DiagnosticCode::UnreachableRule)
            .expect("redundant row");
        assert_eq!(d.severity, Severity::Hint);
        assert_eq!(d.args.get("redundant").map(String::as_str), Some("true"));
        assert_eq!(d.args.get("rowId").map(String::as_str), Some("r2"));
        assert!(d.message.contains("redundant"), "{}", d.message);
    }
}

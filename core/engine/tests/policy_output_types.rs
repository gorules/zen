use serde_json::json;
use zen_engine::policy::{Cursor, CursorTarget, NlExpression, PolicyWorkspace, ScopeRequest};
use zen_expression::nl::{EditHint, NlTokenKind};

fn tier_dictionary() -> serde_json::Value {
    json!({
        "id": "dict1",
        "type": "dictionary",
        "props": { "data": {
            "name": "customerTier",
            "entries": [
                { "id": "e1", "value": "VIP", "label": "Very important" },
                { "id": "e2", "value": "STD", "label": "Standard" }
            ]
        }}
    })
}

fn table_with_output(column_type: &str, cells: &[&str]) -> serde_json::Value {
    let rules: Vec<serde_json::Value> = cells
        .iter()
        .enumerate()
        .map(|(i, cell)| json!({ "_id": format!("row{i}"), "in1": if i == 0 { "" } else { "> 10" }, "out1": cell }))
        .collect();
    json!({
        "id": "dt1",
        "type": "decisionTable",
        "props": { "data": {
            "hitPolicy": "first",
            "inputs": [ { "id": "in1", "name": "Age", "field": "customer.age" } ],
            "outputs": [ { "id": "out1", "name": "Tier", "field": "customer.tier", "type": column_type } ],
            "rules": rules
        }}
    })
}

fn workspace_with(blocks: Vec<serde_json::Value>) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "main",
        serde_json::from_value(json!({ "blocks": blocks })).unwrap(),
    );
    ws
}

fn cell_diagnostics(ws: &PolicyWorkspace) -> Vec<String> {
    ws.diagnostics("main")
        .iter()
        .map(|d| format!("{d:?}"))
        .collect()
}

#[test]
fn number_column_rejects_string_cells() {
    let ws = workspace_with(vec![table_with_output("number", &["42", "'high'"])]);
    let diagnostics = cell_diagnostics(&ws);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("must be `number`") && d.contains("row1")),
        "got: {diagnostics:?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.contains("row0")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn typed_column_accepts_matching_cells() {
    let ws = workspace_with(vec![table_with_output(
        "number",
        &["42", "customer.age * 2"],
    )]);
    let diagnostics = cell_diagnostics(&ws);
    assert!(
        !diagnostics.iter().any(|d| d.contains("TypeMismatch")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn dictionary_column_checks_membership_of_literals() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        table_with_output("customerTier", &["'VIP'", "'GOLD'"]),
    ]);
    let diagnostics = cell_diagnostics(&ws);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("must be `customerTier`") && d.contains("row1")),
        "got: {diagnostics:?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.contains("row0")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn dictionary_array_column_accepts_and_checks_lists() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        table_with_output("customerTier[]", &["['VIP', 'STD']", "['VIP', 'GOLD']"]),
    ]);
    let diagnostics = cell_diagnostics(&ws);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("must be `customerTier[]`") && d.contains("row1")),
        "got: {diagnostics:?}"
    );
    assert!(
        !diagnostics.iter().any(|d| d.contains("row0")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn unknown_dictionary_type_is_diagnosed_on_head() {
    let ws = workspace_with(vec![table_with_output("goldTier", &["'VIP'"])]);
    let diagnostics = cell_diagnostics(&ws);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("unknown output type 'goldTier'")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn malformed_type_annotation_is_diagnosed() {
    let ws = workspace_with(vec![table_with_output("customer tier", &["'VIP'"])]);
    let diagnostics = cell_diagnostics(&ws);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.contains("invalid output type 'customer tier'")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn declared_type_narrows_output_schema() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        table_with_output("customerTier", &["'VIP'", "'STD'"]),
    ]);
    let outputs = ws.outputs(&ScopeRequest {
        policy_path: "main".into(),
        goals: Vec::new(),
    });
    let customer = outputs
        .iter()
        .find(|o| o.path.as_ref() == "customer")
        .unwrap_or_else(|| panic!("output present, got: {outputs:?}"));
    let printed = format!("{:?}", customer.resolved_type);
    assert!(
        printed.contains("Enum(Some(\"customerTier\"), [\"VIP\", \"STD\"])"),
        "expected narrowed enum type, got: {printed}"
    );
}

fn output_cell<'a>(results: &'a [NlExpression], row: &str) -> &'a NlExpression {
    results
        .iter()
        .find(|e| {
            matches!(
                &e.target,
                CursorTarget::DecisionTableCell { row: r, col }
                    if r.as_ref() == row && col.as_ref() == "out1"
            )
        })
        .expect("output cell projected")
}

#[test]
fn nl_output_cell_gets_enum_select_with_labels() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        table_with_output("customerTier", &["'VIP'"]),
    ]);
    let results = ws.nl("main");
    let cell = output_cell(&results, "row0");

    let token = &cell.result.tokens[0];
    assert!(matches!(token.token, NlTokenKind::Str { .. }));
    let EditHint::Select { options } = token.hint.clone().expect("select hint") else {
        panic!("expected select hint, got {:?}", token.hint);
    };
    let options = &cell.result.enums[options as usize];
    assert_eq!(options[0].label, "Very important");
    assert_eq!(options[0].source.as_deref(), Some("\"VIP\""));

    let subject_options = cell.result.subject_options.as_ref().expect("options");
    assert_eq!(subject_options.len(), 2);
}

#[test]
fn nl_array_output_cell_gets_multiselect() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        table_with_output("customerTier[]", &["['VIP', 'STD']"]),
    ]);
    let results = ws.nl("main");
    let cell = output_cell(&results, "row0");

    let token = &cell.result.tokens[0];
    assert!(matches!(token.token, NlTokenKind::EnumList { .. }));
    assert!(matches!(token.hint, Some(EditHint::MultiSelect { .. })));
}

#[test]
fn nl_tokenize_live_output_cell_uses_declared_type() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        table_with_output("customerTier", &["'VIP'"]),
    ]);
    let result = ws
        .nl_tokenize(
            &Cursor {
                policy_path: "main".into(),
                block_id: "dt1".into(),
                pos: 0,
                target: CursorTarget::DecisionTableCell {
                    row: "row0".into(),
                    col: "out1".into(),
                },
            },
            "'STD'",
        )
        .expect("tokenized");

    let token = &result.tokens[0];
    assert!(matches!(token.hint, Some(EditHint::Select { .. })));
    let subject_options = result.subject_options.as_ref().expect("options");
    assert_eq!(subject_options[1].label, "Standard");
}

#[test]
fn untyped_columns_keep_inferred_behavior() {
    let ws = workspace_with(vec![table_with_output("", &["'a'", "'b'"])]);
    let diagnostics = cell_diagnostics(&ws);
    assert!(
        !diagnostics.iter().any(|d| d.contains("TypeMismatch")),
        "got: {diagnostics:?}"
    );

    let results = ws.nl("main");
    let cell = output_cell(&results, "row0");
    assert!(cell.result.tokens[0].hint.is_none());
    assert!(cell.result.subject_options.is_none());
}

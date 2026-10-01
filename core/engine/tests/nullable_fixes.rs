use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{Diagnostic, DiagnosticCode, PolicyWorkspace, Workspace};

fn mismatches(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .filter(|d| d.code == DiagnosticCode::TypeMismatch)
        .collect()
}

fn arg(diagnostic: &Diagnostic, key: &str) -> Option<String> {
    diagnostic.args.get(key).cloned()
}

fn policy(expression: &str, table: bool) -> Value {
    let mut blocks = vec![
        json!({ "id": "dm", "type": "dataModel", "props": { "data": {
        "name": "applicant",
        "properties": [
            { "id": "p1", "name": "target", "type": "number", "array": false, "optional": true },
            { "id": "p2", "name": "age", "type": "number", "array": false, "optional": false }
        ]
    } } }),
    ];
    if table {
        blocks.push(
            json!({ "id": "dt", "type": "decisionTable", "props": { "data": {
            "hitPolicy": "first",
            "inputs": [ { "id": "i0", "name": "", "field": "applicant.age" } ],
            "outputs": [ { "id": "o0", "name": "", "field": "applicant.rate" } ],
            "rules": [ { "_id": "r1", "i0": ">= 18", "o0": "0.1" } ]
        } } }),
        );
    }
    blocks.push(
        json!({ "id": "calc", "type": "expression", "props": { "data": {
        "key": "applicant.result", "value": expression
    } } }),
    );
    json!({ "blocks": blocks })
}

fn policy_mismatches(expression: &str, table: bool) -> Vec<Diagnostic> {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(policy(expression, table)).expect("policy"),
    );
    mismatches(ws.diagnostics("p"))
}

#[test]
fn optional_input_offers_default_and_points_at_the_declaration() {
    let found = policy_mismatches("applicant.target > 0 ? 1 : 0", false);
    assert_eq!(found.len(), 1, "{found:?}");
    let d = &found[0];
    assert_eq!(arg(d, "nullablePath").as_deref(), Some("applicant.target"));
    assert_eq!(arg(d, "sourceId").as_deref(), Some("dm"));
    let fixed = arg(d, "fixSource").expect("fix");
    assert_eq!(fixed, "(applicant.target ?? 0) > 0 ? 1 : 0");
    assert!(policy_mismatches(&fixed, false).is_empty());
}

#[test]
fn uncovered_table_output_points_at_the_table() {
    let found = policy_mismatches("applicant.rate * 2", true);
    assert_eq!(found.len(), 1, "{found:?}");
    let d = &found[0];
    assert_eq!(arg(d, "nullablePath").as_deref(), Some("applicant.rate"));
    assert_eq!(arg(d, "sourceId").as_deref(), Some("dt"));
    let fixed = arg(d, "fixSource").expect("fix");
    assert_eq!(fixed, "(applicant.rate ?? 0) * 2");
    assert!(policy_mismatches(&fixed, true).is_empty());
}

#[test]
fn nullable_divisor_gets_no_default() {
    let found = policy_mismatches("10 / applicant.target", false);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(arg(&found[0], "fixSource"), None);
    assert_eq!(arg(&found[0], "sourceId").as_deref(), Some("dm"));

    let found = policy_mismatches("applicant.target / 10", false);
    assert_eq!(
        arg(&found[0], "fixSource").as_deref(),
        Some("(applicant.target ?? 0) / 10")
    );
}

fn graph(middle: Vec<Value>, edges: Vec<(&str, &str)>) -> DecisionContent {
    let schema = json!({
        "type": "object",
        "properties": { "target": { "type": "number" }, "age": { "type": "number" } },
        "required": ["age"]
    });
    let mut nodes = vec![
        json!({ "id": "in", "name": "in", "type": "inputNode", "content": { "schema": schema.to_string() } }),
    ];
    nodes.extend(middle);
    nodes.push(json!({ "id": "out", "name": "out", "type": "outputNode", "content": {} }));
    let edges: Vec<Value> = edges
        .iter()
        .enumerate()
        .map(|(i, (a, b))| json!({ "id": format!("e{i}"), "sourceId": a, "targetId": b, "sourceHandle": null }))
        .collect();
    serde_json::from_value(json!({ "nodes": nodes, "edges": edges })).expect("graph")
}

fn expression(id: &str, value: &str) -> Value {
    json!({ "id": id, "name": id, "type": "expressionNode", "content": {
        "expressions": [ { "id": format!("{id}-x"), "key": "result", "value": value } ],
        "passThrough": true
    } })
}

fn graph_mismatches(content: DecisionContent) -> Vec<Diagnostic> {
    let mut ws = Workspace::new();
    ws.set_document("g", content);
    mismatches(ws.diagnostics("g"))
}

#[test]
fn graph_input_and_upstream_table_sources() {
    let found = graph_mismatches(graph(
        vec![expression("calc", "target + 1")],
        vec![("in", "calc"), ("calc", "out")],
    ));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(arg(&found[0], "sourceId").as_deref(), Some("in"));
    assert_eq!(
        arg(&found[0], "fixSource").as_deref(),
        Some("(target ?? 0) + 1")
    );

    let table = json!({ "id": "dt", "name": "dt", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "inputs": [ { "id": "i0", "name": "Age", "field": "age" } ],
        "outputs": [ { "id": "o0", "name": "Rate", "field": "rate" } ],
        "rules": [ { "_id": "r1", "i0": ">= 18", "o0": "0.1" } ],
        "passThrough": true
    } });
    let found = graph_mismatches(graph(
        vec![table, expression("calc", "rate * 2")],
        vec![("in", "dt"), ("dt", "calc"), ("calc", "out")],
    ));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(arg(&found[0], "nullablePath").as_deref(), Some("rate"));
    assert_eq!(arg(&found[0], "sourceId").as_deref(), Some("dt"));
}

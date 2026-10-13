//! An unknown member of an entity names the entity, and a windowed
//! feature's name as written lists the windows to read instead: in rules,
//! graph nodes, features, computes, calls and outcomes; through references
//! and closure aliases. Other objects keep the generic message.

use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::model::DecisionContent;
use zen_engine::policy::{
    CursorTarget, Diagnostic, DiagnosticCode, PolicyWorkspace, Severity, Workspace,
};

const TWO: &str = "`txn_count` has windows: read `txn_count_7d` or `txn_count_30d`";
const THREE: &str = "`spend` has windows: read `spend_1d`, `spend_7d` or `spend_30d`";

/// Transactions (events) reference a card and a customer; the card has
/// windowed features; the customer's feature aggregates transactions.
fn models(customer_feature: &str) -> Vec<Value> {
    vec![
        json!({ "id": "dm-transaction", "type": "dataModel", "props": { "data": {
            "name": "transaction",
            "events": { "id": "txn_id", "time": "authorized_at" },
            "properties": [
                { "id": "t1", "name": "txn_id", "type": "string" },
                { "id": "t2", "name": "amount", "type": "number" },
                { "id": "t3", "name": "card", "type": "reference", "target": "card" },
                { "id": "t4", "name": "customer", "type": "reference", "target": "customer" }
            ]
        } } }),
        json!({ "id": "dm-card", "type": "dataModel", "props": { "data": {
            "name": "card",
            "reference": {},
            "properties": [
                { "id": "c1", "name": "id", "type": "string" },
                { "id": "c2", "name": "txn_count", "type": "number",
                  "feature": { "expr": "count(transaction)", "window": ["7d", "30d"], "default": 0 } },
                { "id": "c3", "name": "spend", "type": "number",
                  "feature": { "expr": "sum(transaction as t, t.amount)", "window": ["1d", "7d", "30d"], "default": 0 } }
            ]
        } } }),
        json!({ "id": "dm-customer", "type": "dataModel", "props": { "data": {
            "name": "customer",
            "reference": {},
            "properties": [
                { "id": "u1", "name": "id", "type": "string" },
                { "id": "u2", "name": "card_txns", "type": "number",
                  "feature": { "expr": customer_feature, "window": "7d", "default": 0 } }
            ]
        } } }),
    ]
}

fn policy(rule: &str, customer_feature: &str) -> Value {
    let mut blocks = models(customer_feature);
    blocks.push(
        json!({ "id": "rule", "type": "expression", "props": { "data": {
        "key": "transaction.flag", "value": rule
    } } }),
    );
    json!({ "blocks": blocks })
}

fn workspace(doc: Value) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws
}

fn rule(expr: &str) -> PolicyWorkspace {
    workspace(policy(expr, "count(transaction)"))
}

fn unknowns(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .filter(|d| d.code == DiagnosticCode::UndefinedVariable)
        .collect()
}

fn messages(ws: &PolicyWorkspace) -> Vec<String> {
    unknowns(ws.diagnostics("p"))
        .into_iter()
        .map(|d| d.message)
        .collect()
}

#[test]
fn a_rule_reading_a_windowed_feature_as_written_lists_its_windows() {
    let source = "transaction.card.txn_count > 3";
    let found = unknowns(rule(source).diagnostics("p"));
    assert_eq!(found.len(), 1, "{found:#?}");
    let d = &found[0];
    assert_eq!(d.message, TWO);
    assert_eq!(d.severity, Severity::Error);
    assert_eq!(d.location.block_id.as_deref(), Some("rule"));
    let (start, end) = d.location.span.expect("at the read");
    assert_eq!(
        &source[start as usize..end as usize],
        "transaction.card.txn_count"
    );

    // Three windows; `$root`.
    assert_eq!(messages(&rule("transaction.card.spend > 3")), vec![THREE]);
    assert_eq!(
        messages(&rule("$root.transaction.card.txn_count > 3")),
        vec![TWO]
    );
}

#[test]
fn any_other_unknown_member_of_an_entity_names_it() {
    assert_eq!(
        messages(&rule("transaction.card.foo > 3")),
        vec!["`card` has no `foo`"]
    );
    assert_eq!(
        messages(&rule("transaction.amout > 3")),
        vec!["`transaction` has no `amout`"]
    );
    // Not an entity: the generic message.
    let mut doc = policy("risk.levl > 0", "count(transaction)");
    doc["blocks"].as_array_mut().unwrap().insert(
        3,
        json!({ "id": "risk", "type": "expression",
        "props": { "data": { "key": "risk", "value": "{ level: 1 }" } } }),
    );
    let found = messages(&workspace(doc));
    assert_eq!(found, vec!["'levl' is not a valid member of `object`."]);
}

#[test]
fn feature_compute_and_call_expressions_are_explained_too() {
    // Through a closure alias in a feature: `t.card` is the card.
    let ws = workspace(policy("true", "sum(transaction as t, t.card.txn_count)"));
    let found = unknowns(ws.diagnostics("p"));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].message, TWO);
    assert_eq!(
        serde_json::to_value(found[0].location.target.as_ref().unwrap()).unwrap(),
        serde_json::to_value(CursorTarget::FeatureExpr {
            id: Arc::from("u2")
        })
        .unwrap()
    );

    // A compute and a call on the transaction, from its own fields.
    let mut doc = policy("true", "count(transaction)");
    let props = doc["blocks"][0]["props"]["data"]["properties"]
        .as_array_mut()
        .unwrap();
    props.push(
        json!({ "id": "t6", "name": "scaled", "type": "number", "compute": "card.spend * 2" }),
    );
    props.push(json!({ "id": "t7", "name": "score", "type": "number",
        "model": { "datasource": "fraud", "request": "{ n: card.txn_count }", "when": "card.bar > 0" } }));
    let mut found = messages(&workspace(doc));
    found.sort();
    assert_eq!(found, vec!["`card` has no `bar`", THREE, TWO], "{found:?}");
}

#[test]
fn an_outcome_expression_is_explained_too() {
    let mut doc = policy("true", "count(transaction)");
    doc["blocks"][0]["props"]["data"]["properties"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "t6", "name": "heavy", "type": "boolean",
            "outcome": { "expr": "card.txn_count > 5" } }));
    let found = unknowns(workspace(doc).diagnostics("p"));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].message, TWO);
    assert_eq!(
        serde_json::to_value(found[0].location.target.as_ref().unwrap()).unwrap(),
        serde_json::to_value(CursorTarget::OutcomeExpr {
            id: Arc::from("t6")
        })
        .unwrap()
    );
}

#[test]
fn a_graph_node_on_an_entity_request_is_explained_too() {
    let node = |id: &str, kind: &str, content: Value| json!({ "id": id, "name": id, "type": kind, "content": content });
    let edge = |id: &str, source: &str, target: &str| json!({ "id": id, "sourceId": source, "targetId": target, "sourceHandle": null });
    let graph = json!({
        "imports": ["models"],
        "nodes": [
            node("in", "inputNode", json!({ "target": "transaction" })),
            node("ex", "expressionNode", json!({ "expressions": [
                { "id": "x0", "key": "a", "value": "card.txn_count > 3" },
                { "id": "x1", "key": "b", "value": "card.foo" },
                { "id": "x2", "key": "c", "value": "$root.card.spend > 1" }
            ] })),
            node("out", "outputNode", json!({}))
        ],
        "edges": [edge("e1", "in", "ex"), edge("e2", "ex", "out")]
    });
    let document = |value: Value| -> DecisionContent { serde_json::from_value(value).unwrap() };
    let mut ws = Workspace::new();
    ws.set_document(
        "models",
        document(json!({ "imports": [], "blocks": models("count(transaction)") })),
    );
    ws.set_document("g", document(graph));
    let mut found: Vec<(String, String)> = unknowns(ws.diagnostics("g"))
        .into_iter()
        .map(|d| {
            (
                d.location
                    .expression_id
                    .as_deref()
                    .unwrap_or_default()
                    .to_string(),
                d.message,
            )
        })
        .collect();
    found.sort();
    assert_eq!(found.len(), 3, "{found:#?}");
    assert_eq!(found[0], ("x0".to_string(), TWO.to_string()));
    assert_eq!(
        found[1],
        ("x1".to_string(), "`card` has no `foo`".to_string())
    );
    assert_eq!(found[2], ("x2".to_string(), THREE.to_string()));

    // A request typed by a schema: no entity, the generic message.
    let schema = json!({
        "type": "object",
        "properties": { "customer": { "type": "object", "properties": { "age": { "type": "number" } }, "required": ["age"] } },
        "required": ["customer"]
    });
    let graph = json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": schema.to_string() })),
            node("ex", "expressionNode", json!({ "expressions": [
                { "id": "x0", "key": "a", "value": "customer.agee > 3" }
            ] })),
            node("out", "outputNode", json!({}))
        ],
        "edges": [edge("e1", "in", "ex"), edge("e2", "ex", "out")]
    });
    let mut ws = Workspace::new();
    ws.set_document("g", document(graph));
    let found: Vec<String> = unknowns(ws.diagnostics("g"))
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert_eq!(found, vec!["'agee' is not a valid member of `object`."]);
}

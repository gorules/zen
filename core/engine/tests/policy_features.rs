//! Features on data model properties: a property with `feature` is a value
//! the host supplies (the feature store), one optional input property per
//! window. Without `feature` nothing changes.

use serde_json::json;
use std::sync::Arc;
use zen_engine::policy::{
    Cursor, CursorTarget, EvaluateRequest, EvaluationError, PolicyWorkspace, ScopeRequest,
    Severity,
};
use zen_expression::variable::{Variable, VariableType};

/// A card fraud model: transactions (events) reference a card; the card has
/// reference data and windowed features.
fn fraud(expr: &str) -> serde_json::Value {
    json!({
        "blocks": [
            { "id": "dm-transaction", "type": "dataModel", "props": { "data": {
                "name": "transaction",
                "events": { "id": "txn_id", "time": "authorized_at" },
                "properties": [
                    { "id": "t1", "name": "txn_id", "type": "string" },
                    { "id": "t2", "name": "amount", "type": "number" },
                    { "id": "t3", "name": "card", "type": "reference", "target": "card" }
                ]
            } } },
            { "id": "dm-card", "type": "dataModel", "props": { "data": {
                "name": "card",
                "reference": { "validFrom": "valid_from" },
                "properties": [
                    { "id": "c1", "name": "id", "type": "string" },
                    { "id": "c2", "name": "holder_country", "type": "string" },
                    { "id": "c3", "name": "txn_count", "type": "number",
                      "feature": { "expr": "count(transaction)", "window": ["1h", "7d"] } },
                    { "id": "c4", "name": "spend_1d", "type": "number",
                      "feature": { "expr": "sum(transaction as t, t.amount)", "window": "1d", "default": 0 } }
                ]
            } } },
            { "id": "burst", "type": "expression", "props": { "data": {
                "key": "transaction.burst", "value": expr
            } } }
        ]
    })
}

fn workspace(expr: &str) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(fraud(expr)).unwrap());
    ws
}

fn evaluate(ws: &PolicyWorkspace, input: serde_json::Value) -> Result<serde_json::Value, EvaluationError> {
    ws.evaluate(&EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(input),
        goals: Vec::new(),
        trace: false,
    })
    .map(|r| r.output.into())
}

#[test]
fn feature_windows_are_inputs() {
    // No default: nullable, handled by the rule; with one: a plain number.
    let ws = workspace("(transaction.card.txn_count_1h ?? 0) > 3 or transaction.card.spend_1d > 100");
    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");

    let card = ws
        .entities(&ScopeRequest::for_policy("p"))
        .into_iter()
        .find(|e| e.name.as_ref() == "card")
        .expect("card");
    let fields: Vec<&str> = card.fields.iter().map(|f| f.name.as_ref()).collect();
    for name in ["txn_count_1h", "txn_count_7d", "spend_1d"] {
        assert!(fields.contains(&name), "{name} in {fields:?}");
    }
    // Not the property as written: only its windows exist.
    assert!(!fields.contains(&"txn_count"), "{fields:?}");
    let spend = card.fields.iter().find(|f| f.name.as_ref() == "spend_1d").unwrap();
    assert!(matches!(spend.resolved_type, VariableType::Number), "{:?}", spend.resolved_type);
}

#[test]
fn a_feature_without_default_must_be_handled_as_nullable() {
    let ws = workspace("transaction.card.txn_count_1h > 3");
    let messages: Vec<String> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.to_string())
        .collect();
    assert!(messages.iter().any(|m| m.contains("number?")), "{messages:?}");
}

#[test]
fn a_policy_reads_features_from_the_pool() {
    let ws = workspace("transaction.card.txn_count_1h > 3");
    let out = evaluate(
        &ws,
        json!({
            "transaction": { "txn_id": "T1", "amount": 10, "card": "C1" },
            "card": [{ "id": "C1", "holder_country": "GB", "txn_count_1h": 4, "txn_count_7d": 9, "spend_1d": 120 }]
        }),
    )
    .expect("evaluates");
    assert_eq!(out["transaction"]["burst"], json!(true), "{out}");
}

#[test]
fn a_feature_not_supplied_is_null_not_an_error() {
    // Not covered (no history): the host leaves it out; rules see null.
    let ws = workspace("transaction.card.txn_count_1h == null");
    let out = evaluate(
        &ws,
        json!({
            "transaction": { "txn_id": "T1", "amount": 10, "card": "C1" },
            "card": [{ "id": "C1", "holder_country": "GB" }]
        }),
    )
    .expect("evaluates");
    assert_eq!(out["transaction"]["burst"], json!(true), "{out}");
}

#[test]
fn a_referenced_entity_missing_from_the_pool_fails_closed() {
    let ws = workspace("transaction.card.txn_count_1h > 3");
    let err = evaluate(
        &ws,
        json!({ "transaction": { "txn_id": "T1", "amount": 10, "card": "C1" } }),
    )
    .expect_err("the card is not in the pool");
    assert!(matches!(err, EvaluationError::InputValidationFailed { .. }), "{err:?}");
}

#[test]
fn completions_offer_feature_windows_through_a_reference() {
    let expr = "transaction.card.";
    let ws = workspace(expr);
    let completions = ws.completions(&Cursor {
        policy_path: Arc::from("p"),
        block_id: Arc::from("burst"),
        pos: expr.len() as u32,
        target: CursorTarget::Expression { id: Arc::from("s1") },
    });
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    for expected in ["holder_country", "txn_count_1h", "txn_count_7d", "spend_1d"] {
        assert!(labels.contains(&expected), "{expected} in {labels:?}");
    }
    assert!(!labels.contains(&"txn_count"), "{labels:?}");
}

#[test]
fn a_bad_window_or_a_clash_is_a_diagnostic() {
    let mut doc = fraud("true");
    let props = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
    props[2]["feature"]["window"] = json!(["1h", "07d"]);
    props.push(json!({ "id": "c5", "name": "spend_1d", "type": "number" }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let messages: Vec<String> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.to_string())
        .collect();
    assert!(messages.iter().any(|m| m.contains("window '07d'")), "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("duplicate property 'spend_1d'")), "{messages:?}");
}

#[test]
fn feature_attributes_round_trip() {
    let doc = fraud("true");
    let parsed: zen_engine::policy::PolicyDocument = serde_json::from_value(doc.clone()).unwrap();
    let back = serde_json::to_value(&parsed).unwrap();
    let card = &back["blocks"][1]["props"]["data"];
    assert_eq!(card["reference"], json!({ "validFrom": "valid_from" }));
    assert_eq!(back["blocks"][0]["props"]["data"]["events"], json!({ "id": "txn_id", "time": "authorized_at" }));
    assert_eq!(
        card["properties"][3]["feature"],
        json!({ "expr": "sum(transaction as t, t.amount)", "window": "1d", "default": 0 })
    );
}

#[test]
fn exact_types_and_computed_properties_type_check() {
    let doc = json!({
        "blocks": [
            { "id": "dm-transaction", "type": "dataModel", "props": { "data": {
                "name": "transaction",
                "events": { "id": "txn_id", "time": "authorized_at" },
                "key": "txn_id",
                "sources": { "history": { "datasource": "warehouse", "table": "ANALYTICS.TRANSACTIONS" },
                             "stream": { "datasource": "payments", "topic": "transactions" } },
                "rebuild": { "at": "04:00", "days": 3 },
                "sparse": false,
                "properties": [
                    { "id": "t1", "name": "txn_id", "type": "string", "column": "TXN_ID" },
                    { "id": "t2", "name": "authorized_at", "type": "timestamp" },
                    { "id": "t3", "name": "amount", "type": "decimal", "scale": 2 },
                    { "id": "t4", "name": "items", "type": "integer" },
                    { "id": "t5", "name": "fx_rate", "type": "decimal" },
                    { "id": "t6", "name": "amount_gbp", "type": "decimal", "scale": 2, "compute": "amount * fx_rate" }
                ]
            } } },
            { "id": "big", "type": "expression", "props": { "data": {
                "key": "transaction.big", "value": "transaction.amount_gbp > 1000 and transaction.items >= 1"
            } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc.clone()).unwrap());
    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let out = evaluate(
        &ws,
        json!({ "transaction": {
            "txn_id": "T1", "authorized_at": "2026-02-14T10:00:00Z", "amount": 900,
            "items": 2, "fx_rate": 1.2, "amount_gbp": 1080
        } }),
    )
    .expect("evaluates");
    assert_eq!(out["transaction"]["big"], json!(true), "{out}");

    // Round trip keeps the new attributes and types.
    let parsed: zen_engine::policy::PolicyDocument = serde_json::from_value(doc.clone()).unwrap();
    let back = serde_json::to_value(&parsed).unwrap();
    let (data, sent) = (&back["blocks"][0]["props"]["data"], &doc["blocks"][0]["props"]["data"]);
    for field in ["events", "key", "sources", "rebuild", "sparse"] {
        assert_eq!(data[field], sent[field], "{field}");
    }
    for (prop, sent) in data["properties"].as_array().unwrap().iter().zip(sent["properties"].as_array().unwrap()) {
        for field in ["type", "scale", "compute", "column"] {
            assert_eq!(prop[field], sent[field], "{field} of {}", sent["name"]);
        }
    }
}

#[test]
fn a_model_output_is_nullable_unless_required() {
    let doc = |required: bool| {
        json!({ "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "transaction",
                "properties": [
                    { "id": "t1", "name": "amount", "type": "number" },
                    { "id": "t2", "name": "fraud_score", "type": "number", "required": required,
                      "model": { "datasource": "fraud", "inputs": { "amount": "transaction.amount" } } }
                ] } } },
            { "id": "e", "type": "expression", "props": { "data": {
                "key": "transaction.risky", "value": "transaction.fraud_score > 0.8" } } }
        ] })
    };
    let errors = |required: bool| {
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("p", serde_json::from_value(doc(required)).unwrap());
        ws.diagnostics("p").into_iter().filter(|d| d.severity == Severity::Error).count()
    };
    assert!(errors(false) > 0, "nullable: `> 0.8` needs null handled");
    assert_eq!(errors(true), 0, "required: always a number");
}

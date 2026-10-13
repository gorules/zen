//! Features on data model properties: a property with `feature` is a value
//! the host supplies (the feature store), one optional input property per
//! window. Without `feature` nothing changes.

use serde_json::json;
use std::sync::Arc;
use zen_engine::policy::{
    Cursor, CursorTarget, EvaluateRequest, EvaluationError, PolicyWorkspace, ScopeRequest, SuppliedBy,
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
                      "feature": { "expr": "count(transaction)", "window": ["1h", "7d", "all"] } },
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
    for name in ["txn_count_1h", "txn_count_7d", "txn_count_all", "spend_1d"] {
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
fn types_and_computed_properties_type_check() {
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
                    { "id": "t2", "name": "authorized_at", "type": "date" },
                    { "id": "t3", "name": "amount", "type": "number" },
                    { "id": "t4", "name": "items", "type": "number" },
                    { "id": "t5", "name": "fx_rate", "type": "number" },
                    { "id": "t6", "name": "amount_gbp", "type": "number", "compute": "amount * fx_rate" }
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
        for field in ["type", "compute", "column"] {
            assert_eq!(prop[field], sent[field], "{field} of {}", sent["name"]);
        }
    }

    // Old type names still load and evaluate the same, with a warning each;
    // a `scale` is dropped.
    let mut old = doc.clone();
    let props = old["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    props[1]["type"] = json!("timestamp");
    props[2]["type"] = json!("decimal");
    props[2]["scale"] = json!(2);
    props[3]["type"] = json!("integer");
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(old.clone()).unwrap());
    let found = ws.diagnostics("p");
    assert!(found.iter().all(|d| d.severity != Severity::Error), "{found:#?}");
    for message in [
        "`authorized_at`: `timestamp` is an old type name; use `date`",
        "`amount`: `decimal` is an old type name; use `number`",
        "`items`: `integer` is an old type name; use `number`",
    ] {
        assert!(
            found.iter().any(|d| d.severity == Severity::Warning && d.message == message),
            "{message} in {found:#?}"
        );
    }
    let out = evaluate(
        &ws,
        json!({ "transaction": {
            "txn_id": "T1", "authorized_at": "2026-02-14T10:00:00Z", "amount": 900,
            "items": 2, "fx_rate": 1.2, "amount_gbp": 1080
        } }),
    )
    .expect("evaluates");
    assert_eq!(out["transaction"]["big"], json!(true), "{out}");
    let parsed: zen_engine::policy::PolicyDocument = serde_json::from_value(old).unwrap();
    let back = serde_json::to_value(&parsed).unwrap();
    assert!(back["blocks"][0]["props"]["data"]["properties"][2].get("scale").is_none());
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

/// The fraud model with a derived feature, a per-event compute and a model:
/// expressions the host evaluates, edited in the data model.
fn host_expressions(derived: &str, compute: &str, model_input: &str, when: &str) -> PolicyWorkspace {
    let mut doc = fraud("true");
    let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    txn.push(json!({ "id": "t4", "name": "fx_rate", "type": "number" }));
    txn.push(json!({ "id": "t5", "name": "amount_gbp", "type": "number", "compute": compute }));
    txn.push(json!({ "id": "t6", "name": "fraud_score", "type": "number",
        "model": { "datasource": "fraud", "inputs": { "amount": model_input }, "when": when } }));
    let card = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
    card.push(json!({ "id": "c5", "name": "burst_ratio", "type": "number",
        "feature": { "expr": derived } }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws
}

fn cursor(block: &str, pos: usize, target: CursorTarget) -> Cursor {
    Cursor {
        policy_path: Arc::from("p"),
        block_id: Arc::from(block),
        pos: pos as u32,
        target,
    }
}

fn labels(ws: &PolicyWorkspace, cursor: &Cursor) -> Vec<String> {
    ws.completions(cursor).into_iter().map(|c| c.label).collect()
}

#[test]
fn a_windowed_feature_reads_events_with_the_aggregates() {
    let ws = workspace("true");
    let mut doc = fraud("true");
    let source = "sum(transaction as t, t.";
    doc["blocks"][1]["props"]["data"]["properties"][3]["feature"]["expr"] = json!(source);
    let mut ws_typing = PolicyWorkspace::new();
    ws_typing.set_policy("p", serde_json::from_value(doc).unwrap());

    let at_start = labels(&ws, &cursor("dm-card", 0, CursorTarget::FeatureExpr { id: Arc::from("c3") }));
    for expected in ["transaction", "count", "sum", "countDistinct", "params"] {
        assert!(at_start.iter().any(|l| l == expected), "{expected} in {at_start:?}");
    }
    // Not a request field: the card's own features aren't events.
    assert!(!at_start.iter().any(|l| l == "holder_country"), "{at_start:?}");

    let members = labels(
        &ws_typing,
        &cursor("dm-card", source.len(), CursorTarget::FeatureExpr { id: Arc::from("c4") }),
    );
    assert!(members.iter().any(|l| l == "amount"), "{members:?}");
}

#[test]
fn a_derived_feature_reads_the_entity_and_shows_feature_details() {
    let ws = host_expressions("", "amount * fx_rate", "transaction.amount", "transaction.amount > 0");
    let completions = ws.completions(&cursor("dm-card", 0, CursorTarget::FeatureExpr { id: Arc::from("c5") }));
    let count = completions
        .iter()
        .find(|c| c.label == "txn_count_1h")
        .unwrap_or_else(|| panic!("{completions:#?}"));
    assert_eq!(count.detail, "number? · count(transaction) · 1h");
    let spend = completions.iter().find(|c| c.label == "spend_1d").unwrap();
    assert_eq!(spend.detail, "number · sum(transaction as t, t.amount) · 1d");
    assert!(completions.iter().any(|c| c.label == "holder_country"));
    assert!(!completions.iter().any(|c| c.label == "transaction"), "not the events");
}

#[test]
fn compute_and_model_expressions_complete() {
    let ws = host_expressions("txn_count_1h ?? 0", "", "", "");
    let own = labels(&ws, &cursor("dm-transaction", 0, CursorTarget::ComputeExpr { id: Arc::from("t5") }));
    for expected in ["amount", "fx_rate", "card"] {
        assert!(own.iter().any(|l| l == expected), "{expected} in {own:?}");
    }
    let request = labels(
        &ws,
        &cursor("dm-transaction", 0, CursorTarget::ModelInput { id: Arc::from("t6"), input: Arc::from("amount") }),
    );
    assert!(request.iter().any(|l| l == "transaction"), "{request:?}");

    let ws = host_expressions("txn_count_1h ?? 0", "amount", "transaction.", "transaction.");
    for target in [
        CursorTarget::ModelInput { id: Arc::from("t6"), input: Arc::from("amount") },
        CursorTarget::ModelWhen { id: Arc::from("t6") },
    ] {
        let fields = labels(&ws, &cursor("dm-transaction", "transaction.".len(), target));
        assert!(fields.iter().any(|l| l == "amount"), "{fields:?}");
    }
    // Through a reference: the card's features, with their details.
    let ws = host_expressions("txn_count_1h ?? 0", "amount", "transaction.card.", "true");
    let completions = ws.completions(&cursor(
        "dm-transaction",
        "transaction.card.".len(),
        CursorTarget::ModelInput { id: Arc::from("t6"), input: Arc::from("amount") },
    ));
    let count = completions.iter().find(|c| c.label == "txn_count_7d").expect("txn_count_7d");
    assert_eq!(count.detail, "number? · count(transaction) · 7d");
}

#[test]
fn host_expressions_are_diagnosed_where_they_are() {
    let ws = host_expressions(
        "(txn_count_1h ?? 0) / (txn_count_7d ?? 1)",
        "amount * fx_rate",
        "transaction.amount",
        "transaction.amount > 0",
    );
    let errors: Vec<_> = ws.diagnostics("p").into_iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "{errors:#?}");

    let ws = host_expressions("missing_feature > 0", "amount * fx", "transaction.amout", "transaction.amount > 0");
    let errors: Vec<_> = ws.diagnostics("p").into_iter().filter(|d| d.severity == Severity::Error).collect();
    let at = |target: &CursorTarget| {
        errors.iter().any(|d| {
            d.location.target.as_ref().map(|t| serde_json::to_value(t).unwrap())
                == Some(serde_json::to_value(target).unwrap())
        })
    };
    assert!(at(&CursorTarget::FeatureExpr { id: Arc::from("c5") }), "{errors:#?}");
    assert!(at(&CursorTarget::ComputeExpr { id: Arc::from("t5") }), "{errors:#?}");
    assert!(
        at(&CursorTarget::ModelInput { id: Arc::from("t6"), input: Arc::from("amount") }),
        "{errors:#?}"
    );
}

#[test]
fn entities_mark_what_the_host_supplies() {
    let ws = host_expressions("txn_count_1h ?? 0", "amount * fx_rate", "transaction.amount", "true");
    let entities = serde_json::to_value(ws.entities(&ScopeRequest::for_policy("p"))).unwrap();
    let field = |entity: &str, name: &str| {
        entities
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == entity)
            .and_then(|e| e["fields"].as_array().unwrap().iter().find(|f| f["name"] == name))
            .unwrap_or_else(|| panic!("{entity}.{name}"))["origin"]
            .clone()
    };
    assert_eq!(
        field("card", "txn_count_1h")["supply"],
        json!({ "kind": "feature", "base": "txn_count", "expr": "count(transaction)", "window": "1h" })
    );
    assert_eq!(
        field("card", "spend_1d")["supply"],
        json!({ "kind": "feature", "base": "spend_1d", "expr": "sum(transaction as t, t.amount)", "window": "1d", "default": 0 })
    );
    assert_eq!(field("card", "burst_ratio")["supply"]["kind"], "feature");
    assert_eq!(field("transaction", "amount_gbp")["supply"], json!({ "kind": "compute", "expr": "amount * fx_rate" }));
    // Only a type that says more than the engine's (`object`) has an exact type.
    assert!(field("transaction", "amount_gbp").get("exactType").is_none());
    assert_eq!(field("transaction", "fraud_score")["supply"], json!({ "kind": "model", "datasource": "fraud" }));
    assert_eq!(field("transaction", "amount")["origin"], "schema");
    assert!(field("transaction", "amount").get("supply").is_none());
}

#[test]
fn hover_on_a_feature_shows_its_details() {
    let expr = "transaction.card.txn_count_1h > 3";
    let ws = workspace(expr);
    let hover = ws
        .inspect(&cursor("burst", "transaction.card.txn".len(), CursorTarget::Expression { id: Arc::from("s1") }))
        .expect("hover");
    assert_eq!(hover.detail.as_deref(), Some("number? · count(transaction) · 1h"));
}


fn errors_of(doc: serde_json::Value) -> Vec<String> {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws.diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect()
}

#[test]
fn params_come_from_feature_settings() {
    let mut doc = fraud("true");
    doc["blocks"][1]["props"]["data"]["properties"][3]["feature"]["expr"] =
        json!("sum(transaction as t, t.amount, t.amount >= params.high_amount)");
    let without = errors_of(doc.clone());
    assert!(without.iter().any(|m| m.contains("params.high_amount")), "{without:?}");

    doc["blocks"].as_array_mut().unwrap().push(json!({ "id": "fs", "type": "featureSettings", "props": { "data": {
        "timezone": "Europe/London",
        "params": [{ "name": "high_amount", "type": "number", "value": "1000.00" }]
    } } }));
    let with = errors_of(doc);
    assert!(with.is_empty(), "{with:?}");
}

#[test]
fn derived_features_propagate_null() {
    let check = |derived: &str, compute: &str| {
        let mut doc = fraud("true");
        let card = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
        card.push(json!({ "id": "c5", "name": "credit_limit", "type": "number", "optional": true }));
        card.push(json!({ "id": "c6", "name": "ratio", "type": "number", "feature": { "expr": derived } }));
        let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
        txn.push(json!({ "id": "t4", "name": "fx_rate", "type": "number", "optional": true }));
        txn.push(json!({ "id": "t5", "name": "amount_gbp", "type": "number", "compute": compute }));
        errors_of(doc)
    };
    let ok = check(
        "credit_limit > 0 ? spend_1d / credit_limit : null",
        "amount * fx_rate",
    );
    assert!(ok.is_empty(), "{ok:?}");
    let ok = check("txn_count_1h > 0 ? txn_count_1h / (txn_count_7d ?? 1) : null", "amount");
    assert!(ok.is_empty(), "{ok:?}");
    // Still a type error: a string is not a number, null or not.
    let bad = check("holder_country / credit_limit", "amount");
    assert!(!bad.is_empty(), "{bad:?}");
}

#[test]
fn an_all_time_window_is_a_property() {
    let mut doc = fraud("(transaction.card.txn_count_all ?? 0) > 100");
    doc["blocks"][1]["props"]["data"]["properties"][2]["feature"]["window"] = json!(["7d", "all"]);
    assert!(errors_of(doc.clone()).is_empty(), "{:?}", errors_of(doc.clone()));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let card = ws
        .entities(&ScopeRequest::for_policy("p"))
        .into_iter()
        .find(|e| e.name.as_ref() == "card")
        .unwrap();
    let names: Vec<&str> = card.fields.iter().map(|f| f.name.as_ref()).collect();
    assert!(names.contains(&"txn_count_7d") && names.contains(&"txn_count_all"), "{names:?}");
}

#[test]
fn an_input_with_a_default_is_never_null() {
    let doc = |default: serde_json::Value| {
        let mut prop = json!({ "id": "b2", "name": "loyalty", "type": "string", "optional": true });
        if !default.is_null() {
            prop["default"] = default;
        }
        json!({ "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "booking",
                "properties": [{ "id": "b1", "name": "nights", "type": "number" }, prop] } } },
            { "id": "e", "type": "expression", "props": { "data": {
                "key": "booking.label", "value": "booking.loyalty + ' guest'" } } }
        ] })
    };
    assert!(!errors_of(doc(json!(null))).is_empty(), "optional without a default: nullable");
    assert!(errors_of(doc(json!("standard"))).is_empty(), "{:?}", errors_of(doc(json!("standard"))));

    // Kept on round trip, typed as written.
    let sent = doc(json!("standard"));
    let parsed: zen_engine::policy::PolicyDocument = serde_json::from_value(sent).unwrap();
    let back = serde_json::to_value(&parsed).unwrap();
    assert_eq!(back["blocks"][0]["props"]["data"]["properties"][1]["default"], json!("standard"));
}

#[test]
fn defaults_are_filled_before_blocks_run() {
    let doc = json!({ "blocks": [
        { "id": "dm-booking", "type": "dataModel", "props": { "data": {
            "name": "booking",
            "properties": [
                { "id": "b1", "name": "nights", "type": "number", "default": 1 },
                { "id": "b2", "name": "loyalty", "type": "string", "optional": true, "default": "standard" },
                { "id": "b3", "name": "guest", "type": "relationship", "target": "guest" },
                { "id": "b4", "name": "checkin", "type": "date", "optional": true, "default": "2026-01-01" }
            ] } } },
        { "id": "dm-guest", "type": "dataModel", "props": { "data": {
            "name": "guest",
            "properties": [
                { "id": "g1", "name": "tier", "type": "string", "default": "basic" },
                { "id": "g2", "name": "visits", "type": "number",
                  "feature": { "expr": "count(booking)", "window": "all", "default": 0 } }
            ] } } },
        { "id": "e", "type": "expression", "props": { "data": {
            "key": "booking.label",
            "value": "booking.loyalty + ':' + booking.guest.tier + ':' + string(booking.nights + booking.guest.visits_all) + ':' + string(booking.checkin.year())"
        } } }
    ] });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let errors: Vec<_> = ws.diagnostics("p").into_iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "{errors:#?}");

    let input = json!({ "booking": { "loyalty": null, "guest": {} } });
    let out = evaluate(&ws, input.clone()).expect("evaluates");
    assert_eq!(out["booking"]["label"], json!("standard:basic:1:2026"), "{out}");
    // Supplied values win.
    let out = evaluate(
        &ws,
        json!({ "booking": { "nights": 3, "loyalty": "gold", "checkin": "2027-05-01", "guest": { "tier": "vip", "visits_all": 4 } } }),
    )
    .expect("evaluates");
    assert_eq!(out["booking"]["label"], json!("gold:vip:7:2027"), "{out}");
}

#[test]
fn derived_features_read_as_of_and_date_functions() {
    let mut doc = fraud("true");
    let card = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
    card.push(json!({ "id": "c5", "name": "last_txn_at", "type": "date",
        "feature": { "expr": "last(transaction as t, t.authorized_at)", "window": "7d" } }));
    card.push(json!({ "id": "c6", "name": "secs_since_last", "type": "number",
        "feature": { "expr": "last_txn_at_7d == null ? null : d(asOf).diff(d(last_txn_at_7d), 'second')" } }));
    let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    txn.push(json!({ "id": "t4", "name": "authorized_at", "type": "date" }));
    txn.push(json!({ "id": "t5", "name": "hour", "type": "number", "compute": "d(authorized_at).hour()" }));
    let errors = errors_of(doc.clone());
    assert!(errors.is_empty(), "{errors:?}");

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let derived = labels(&ws, &cursor("dm-card", 0, CursorTarget::FeatureExpr { id: Arc::from("c6") }));
    for expected in ["asOf", "d", "last_txn_at_7d"] {
        assert!(derived.iter().any(|l| l == expected), "{expected} in {derived:?}");
    }
    // `asOf` is the read time of derived features only.
    let windowed = labels(&ws, &cursor("dm-card", 0, CursorTarget::FeatureExpr { id: Arc::from("c5") }));
    assert!(!windowed.iter().any(|l| l == "asOf"), "{windowed:?}");
}

#[test]
fn data_model_expressions_have_facts() {
    // Business mode draws pills from `facts`: the expressions on data model
    // properties are listed like any other, not only once focused.
    let ws = host_expressions("txn_count_1h ?? 0", "amount * fx_rate", "transaction.amount", "transaction.amount > 0");
    let targets: Vec<serde_json::Value> = ws
        .facts("p")
        .into_iter()
        .map(|f| serde_json::to_value(&f.target).unwrap())
        .collect();
    for expected in [
        CursorTarget::FeatureExpr { id: Arc::from("c5") },
        CursorTarget::ComputeExpr { id: Arc::from("t5") },
        CursorTarget::ModelInput { id: Arc::from("t6"), input: Arc::from("amount") },
        CursorTarget::ModelWhen { id: Arc::from("t6") },
    ] {
        let expected = serde_json::to_value(&expected).unwrap();
        assert!(targets.contains(&expected), "{expected} in {targets:?}");
    }
}

#[test]
fn a_derived_field_that_needs_its_own_value_is_an_error() {
    let cycles = |ws: &PolicyWorkspace| -> Vec<String> {
        ws.diagnostics("p")
            .into_iter()
            .filter(|d| d.severity == Severity::Error && d.message.contains("itself"))
            .map(|d| d.message)
            .collect()
    };

    // Directly: `burst_ratio = burst_ratio + 5`; and a compute reading itself.
    let ws = host_expressions("burst_ratio + 5", "amount_gbp * 2", "transaction.amount", "true");
    let found = cycles(&ws);
    assert!(found.iter().any(|m| m.contains("`burst_ratio` reads itself")), "{found:#?}");
    assert!(found.iter().any(|m| m.contains("`amount_gbp` reads itself")), "{found:#?}");

    // Through another: `burst_ratio` → `ratio_2` → `burst_ratio`.
    let mut doc = fraud("true");
    let card = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
    card.push(json!({ "id": "c5", "name": "burst_ratio", "type": "number", "feature": { "expr": "ratio_2 * 2" } }));
    card.push(json!({ "id": "c6", "name": "ratio_2", "type": "number", "feature": { "expr": "burst_ratio / 2" } }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let found = cycles(&ws);
    assert!(
        found.iter().any(|m| m.contains("`burst_ratio` depends on itself: burst_ratio → ratio_2 → burst_ratio")),
        "{found:#?}"
    );
    assert_eq!(found.len(), 2, "{found:#?}");

    // Reading other fields and windows is fine.
    let ws = host_expressions("(txn_count_1h ?? 0) / (txn_count_7d ?? 1)", "amount * 2", "transaction.amount", "true");
    assert!(cycles(&ws).is_empty());
}

/// A call written with `request` and `response`: both read the instance, as
/// its derived features do; `response` also reads the reply.
fn call(request: &str, response: &str, when: &str) -> PolicyWorkspace {
    let mut doc = fraud("true");
    let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    txn.push(json!({ "id": "t6", "name": "fraud_score", "type": "number",
        "model": { "datasource": "fraud", "request": request, "response": response, "when": when } }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws
}

#[test]
fn a_call_request_and_response_are_expressions_on_the_instance() {
    let errors = |ws: &PolicyWorkspace| -> Vec<_> {
        ws.diagnostics("p").into_iter().filter(|d| d.severity == Severity::Error).collect()
    };
    let at = |errors: &[zen_engine::policy::Diagnostic], target: CursorTarget| {
        let target = serde_json::to_value(target).unwrap();
        errors
            .iter()
            .any(|d| d.location.target.as_ref().map(|t| serde_json::to_value(t).unwrap()) == Some(target.clone()))
    };

    let ws = call(
        "{ amount, holder: card.holder_country, burst: card.txn_count_1h ?? 0 }",
        "response.result.score * amount",
        "amount > 100",
    );
    assert!(errors(&ws).is_empty(), "{:#?}", errors(&ws));

    // Unknown names, and the request-root paths of the earlier format.
    let ws = call("{ amount: amout }", "respnse.score", "transaction.amount > 100");
    let found = errors(&ws);
    assert!(at(&found, CursorTarget::ModelRequest { id: Arc::from("t6") }), "{found:#?}");
    assert!(at(&found, CursorTarget::ModelResponse { id: Arc::from("t6") }), "{found:#?}");
    assert!(at(&found, CursorTarget::ModelWhen { id: Arc::from("t6") }), "{found:#?}");

    // Completions: the instance's fields; the response also `response`.
    let ws = call("", "", "");
    let request = labels(&ws, &cursor("dm-transaction", 0, CursorTarget::ModelRequest { id: Arc::from("t6") }));
    for expected in ["amount", "card"] {
        assert!(request.iter().any(|l| l == expected), "{expected} in {request:?}");
    }
    let response = labels(&ws, &cursor("dm-transaction", 0, CursorTarget::ModelResponse { id: Arc::from("t6") }));
    for expected in ["response", "amount"] {
        assert!(response.iter().any(|l| l == expected), "{expected} in {response:?}");
    }

    // Facts, so business mode shows their pills.
    let ws = call("{ amount: amount }", "response.score", "amount > 0");
    let targets: Vec<serde_json::Value> =
        ws.facts("p").into_iter().map(|f| serde_json::to_value(&f.target).unwrap()).collect();
    for expected in [
        CursorTarget::ModelRequest { id: Arc::from("t6") },
        CursorTarget::ModelResponse { id: Arc::from("t6") },
        CursorTarget::ModelWhen { id: Arc::from("t6") },
    ] {
        let expected = serde_json::to_value(&expected).unwrap();
        assert!(targets.contains(&expected), "{expected} in {targets:?}");
    }
}

#[test]
fn calls_and_derived_fields_are_ordered_by_what_they_read() {
    let doc = |risk_request: &str, kyc_request: &str, ratio: &str, staleness: &str| {
        let mut doc = fraud("true");
        let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
        txn.push(json!({ "id": "t6", "name": "risk_score", "type": "number",
            "model": { "datasource": "fraud", "request": risk_request, "maxStaleness": staleness } }));
        txn.push(json!({ "id": "t7", "name": "kyc_level", "type": "number",
            "model": { "datasource": "kyc", "request": kyc_request } }));
        txn.push(json!({ "id": "t8", "name": "ratio", "type": "number", "feature": { "expr": ratio } }));
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("p", serde_json::from_value(doc).unwrap());
        ws.diagnostics("p")
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect::<Vec<_>>()
    };

    // A call after another, a derived field after a call, a reuse time: fine.
    let found = doc("{ amount, kyc: kyc_level }", "{ amount }", "risk_score * amount", "5m");
    assert!(found.is_empty(), "{found:#?}");

    // Two calls waiting on each other.
    let found = doc("{ kyc: kyc_level }", "{ risk: risk_score }", "amount", "");
    assert!(
        found.iter().any(|m| m.contains("`risk_score` depends on itself: risk_score → kyc_level → risk_score")),
        "{found:#?}"
    );

    // Compiling stops on it too, not only the editor.
    let mut cyclic = fraud("true");
    let txn = cyclic["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    txn.push(json!({ "id": "t6", "name": "risk_score", "type": "number",
        "model": { "datasource": "fraud", "request": "{ kyc: kyc_level }" } }));
    txn.push(json!({ "id": "t7", "name": "kyc_level", "type": "number",
        "model": { "datasource": "kyc", "request": "{ risk: risk_score }" } }));
    let loader = Arc::new(zen_engine::loader::MemoryLoader::default());
    loader.add(
        "p",
        zen_engine::model::DecisionContent::Policy(zen_engine::model::PolicyContent(Arc::new(
            serde_json::from_value(cyclic).unwrap(),
        ))),
    );
    let engine = zen_engine::DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(
        failures.iter().any(|f| format!("{f:?}").contains("depends on itself")),
        "{failures:#?}"
    );

    // Through a derived field: risk_score → ratio → risk_score.
    let found = doc("{ ratio }", "{ amount }", "risk_score * 2", "");
    assert!(found.iter().any(|m| m.contains("`ratio` depends on itself")), "{found:#?}");

    // A reuse time that is no duration.
    let found = doc("{ amount }", "{ amount }", "amount", "5 minutes");
    assert!(
        found.iter().any(|m| m.contains("`maxStaleness` of call 'risk_score' is a duration like 30s, 5m or 1h, not `5 minutes`")),
        "{found:#?}"
    );
}

#[test]
fn a_call_reads_a_parent_through_root() {
    let doc = |request: &str, when: &str| {
        let mut doc = fraud("true");
        let card = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
        card.push(json!({ "id": "c9", "name": "card_score", "type": "number",
            "model": { "datasource": "fraud", "request": request, "when": when } }));
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("p", serde_json::from_value(doc).unwrap());
        ws
    };
    let errors = |ws: &PolicyWorkspace| -> Vec<String> {
        ws.diagnostics("p")
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.message)
            .collect()
    };

    // The card has no way back to the transaction: `$root` is the request.
    let ws = doc("{ holder_country, amount: $root.transaction.amount }", "$root.transaction.amount > 100");
    assert!(errors(&ws).is_empty(), "{:#?}", errors(&ws));

    // A path the request doesn't have; and the parent without `$root`.
    let found = errors(&doc("{ amount: $root.transaction.amout }", ""));
    assert!(found.iter().any(|m| m.contains("amout")), "{found:#?}");
    let found = errors(&doc("{ amount: transaction.amount }", ""));
    assert!(!found.is_empty(), "a bare parent read is not the instance's: {found:#?}");

    // Completions after `$root.`: the request's entities; `$root` offered once.
    let ws = doc("$root.", "");
    let after_root = labels(&ws, &cursor("dm-card", "$root.".len(), CursorTarget::ModelRequest { id: Arc::from("c9") }));
    assert!(after_root.iter().any(|l| l == "transaction"), "{after_root:?}");
    let ws = doc("", "");
    let fields = labels(&ws, &cursor("dm-card", 0, CursorTarget::ModelRequest { id: Arc::from("c9") }));
    assert!(fields.iter().any(|l| l == "holder_country"), "{fields:?}");
    assert_eq!(fields.iter().filter(|l| *l == "$root").count(), 1, "{fields:?}");
}

#[test]
fn a_property_is_never_shadowed_by_a_name_feature_expressions_add() {
    // `params` and `asOf` are the card's own here, not the settings or the instant.
    let mut doc = fraud("true");
    let card = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
    card.push(json!({ "id": "c7", "name": "params", "type": "number" }));
    card.push(json!({ "id": "c8", "name": "asOf", "type": "string" }));
    card.push(json!({ "id": "c9", "name": "doubled", "type": "number", "feature": { "expr": "params * 2" } }));
    card.push(json!({ "id": "c10", "name": "as_of_len", "type": "number", "feature": { "expr": "len(asOf)" } }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn inputs_say_who_supplies_them_and_whether_they_can_be_left_out() {
    let mut doc = fraud("true");
    let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    txn.push(json!({ "id": "t7", "name": "channel", "type": "string", "default": "card" }));
    txn.push(json!({ "id": "t8", "name": "note", "type": "string", "optional": true }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let inputs = ws.inputs(&ScopeRequest::for_policy("p"));
    let find = |path: &str| inputs.iter().find(|p| p.path.as_ref() == path).unwrap_or_else(|| panic!("{path} in {inputs:#?}"));

    let amount = find("transaction.amount");
    assert!(!amount.optional && amount.supplied_by == SuppliedBy::Request);
    let channel = find("transaction.channel");
    assert!(channel.optional && channel.default == Some(json!("card")));
    assert!(find("transaction.note").optional);
    // The card is reference data with features: the host supplies it.
    let card = find("card");
    assert!(card.supplied_by == SuppliedBy::Host && card.optional, "{card:#?}");
}

/// The fraud model with a call on the transaction: `model` merged into it.
fn with_call(model: serde_json::Value) -> serde_json::Value {
    let mut doc = fraud("true");
    let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    let mut call = json!({ "datasource": "fraud", "request": "{ amount }" });
    call.as_object_mut().unwrap().extend(model.as_object().unwrap().clone());
    txn.push(json!({ "id": "t6", "name": "risk_score", "type": "number", "model": call }));
    doc
}

#[test]
fn call_durations_are_checked() {
    let found = |key: &str, value: serde_json::Value| errors_of(with_call(json!({ key: value })));
    for ok in ["500ms", "2s", "1m", "1h", "1d", ""] {
        assert!(found("timeout", json!(ok)).is_empty(), "timeout {ok}: {:?}", found("timeout", json!(ok)));
    }
    for bad in [json!("0s"), json!("05s"), json!(" 5s"), json!("5 ms"), json!("5"), json!("1w"), json!(500)] {
        let errors = found("timeout", bad.clone());
        assert!(
            errors.iter().any(|m| m.contains("`timeout` of call 'risk_score' is a duration like 500ms")),
            "timeout {bad}: {errors:?}"
        );
    }
    // A reply is reused for seconds or more: no `ms`, no leading zero.
    for ok in ["30s", "5m", "1h", "1d"] {
        assert!(found("maxStaleness", json!(ok)).is_empty(), "maxStaleness {ok}");
    }
    for bad in ["500ms", "05m", "0s", "5 m"] {
        let errors = found("maxStaleness", json!(bad));
        assert!(errors.iter().any(|m| m.contains("`maxStaleness` of call 'risk_score'")), "maxStaleness {bad}: {errors:?}");
    }
    // Windows: minutes, hours and days only.
    let mut doc = fraud("true");
    doc["blocks"][1]["props"]["data"]["properties"][2]["feature"]["window"] = json!(["30s", "10m"]);
    let errors = errors_of(doc);
    assert!(errors.iter().any(|m| m.contains("window '30s'")), "{errors:?}");
    assert!(!errors.iter().any(|m| m.contains("window '10m'")), "{errors:?}");
}

#[test]
fn a_compute_is_null_when_what_it_reads_may_be() {
    let errors = |fx_rate: serde_json::Value, compute: serde_json::Value| {
        let mut doc = fraud("(transaction.amount_gbp > 1000) == true");
        let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
        let mut fx = json!({ "id": "t4", "name": "fx_rate", "type": "number" });
        fx.as_object_mut().unwrap().extend(fx_rate.as_object().unwrap().clone());
        txn.push(fx);
        let mut amount_gbp = json!({ "id": "t5", "name": "amount_gbp", "type": "number" });
        amount_gbp.as_object_mut().unwrap().extend(compute.as_object().unwrap().clone());
        txn.push(amount_gbp);
        errors_of(doc)
    };
    let compute = json!({ "compute": "amount * fx_rate" });
    // `fx_rate?` in, null out: rules handle it.
    assert!(!errors(json!({ "optional": true }), compute.clone()).is_empty());
    assert!(errors(json!({}), compute.clone()).is_empty(), "{:?}", errors(json!({}), compute.clone()));
    // `required` or a `default` says it is always there.
    let required = json!({ "compute": "amount * fx_rate", "required": true });
    assert!(errors(json!({ "optional": true }), required).is_empty());
    let defaulted = json!({ "compute": "amount * fx_rate", "default": 0 });
    assert!(errors(json!({ "optional": true }), defaulted).is_empty());
    // Null itself, or through another compute.
    assert!(!errors(json!({}), json!({ "compute": "amount > 0 ? amount : null" })).is_empty());
    let mut doc = fraud("(transaction.doubled > 1000) == true");
    let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
    txn.push(json!({ "id": "t4", "name": "fx_rate", "type": "number", "optional": true }));
    txn.push(json!({ "id": "t5", "name": "amount_gbp", "type": "number", "compute": "amount * fx_rate" }));
    txn.push(json!({ "id": "t6", "name": "doubled", "type": "number", "compute": "amount_gbp * 2" }));
    assert!(!errors_of(doc).is_empty());
    // Through a reference: another entity's field may be missing.
    assert!(!errors(json!({}), json!({ "compute": "card.holder_country == 'GB' ? 1 : 2" })).is_empty());
}

#[test]
fn a_derivation_loop_through_another_entity_is_an_error() {
    let doc = |card_request: &str| {
        let mut doc = with_call(json!({ "request": "{ amount, card_score: card.card_score }" }));
        let card = doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap();
        card.push(json!({ "id": "c9", "name": "card_score", "type": "number",
            "model": { "datasource": "fraud", "request": card_request } }));
        errors_of(doc)
    };
    let found = doc("{ risk: $root.transaction.risk_score }");
    for expected in [
        "`transaction.risk_score` depends on itself: transaction.risk_score → card.card_score → transaction.risk_score",
        "`card.card_score` depends on itself: card.card_score → transaction.risk_score → card.card_score",
    ] {
        assert!(found.iter().any(|m| m == expected), "{expected} in {found:#?}");
    }
    // Reading the transaction's request data, not what it derives: fine.
    let found = doc("{ amount: $root.transaction.amount }");
    assert!(!found.iter().any(|m| m.contains("itself")), "{found:#?}");

    // Across policies: the card in an import.
    let mut main = with_call(json!({ "request": "{ amount, card_score: card.card_score }" }));
    let card = main["blocks"].as_array_mut().unwrap().remove(1);
    let mut card = json!({ "blocks": [card] });
    card["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap().push(json!({
        "id": "c9", "name": "card_score", "type": "number",
        "model": { "datasource": "fraud", "request": "{ risk: $root.transaction.risk_score }" } }));
    main["imports"] = json!(["cards"]);
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("cards", serde_json::from_value(card).unwrap());
    ws.set_policy("p", serde_json::from_value(main).unwrap());
    let found: Vec<String> = ws.diagnostics("p").into_iter().map(|d| d.message).collect();
    assert!(
        found.iter().any(|m| m.starts_with("`transaction.risk_score` depends on itself")),
        "{found:#?}"
    );
}

#[test]
fn a_list_feature_is_an_array_property() {
    let errors = |prop: serde_json::Value| {
        let mut doc = fraud("true");
        let txn = doc["blocks"][0]["props"]["data"]["properties"].as_array_mut().unwrap();
        txn.push(json!({ "id": "t4", "name": "merchant", "type": "string" }));
        doc["blocks"][1]["props"]["data"]["properties"].as_array_mut().unwrap().push(prop);
        errors_of(doc)
    };
    let top = "topK(transaction as t, t.merchant, 3)";
    let found = errors(json!({ "id": "c5", "name": "top_merchants", "type": "string", "feature": { "expr": top, "window": "7d" } }));
    assert!(
        found.iter().any(|m| m == "`top_merchants` is a list (`topK`): set `array: true` on the property (its `type` is the items')"),
        "{found:#?}"
    );
    let ok = errors(json!({ "id": "c5", "name": "top_merchants", "type": "string", "array": true, "feature": { "expr": top, "window": "7d" } }));
    assert!(ok.is_empty(), "{ok:#?}");
    let found = errors(json!({ "id": "c5", "name": "merchants", "type": "string", "array": true,
        "feature": { "expr": "countDistinct(transaction as t, t.merchant)", "window": "7d" } }));
    assert!(found.iter().any(|m| m.contains("`merchants` is a single value (`countDistinct`)")), "{found:#?}");
    // A selector that gives the whole event is no feature, whatever the property's type.
    for (expr, message) in [
        ("argMax(transaction as t, t.amount)", "`argMax(events, by)` gives the whole event; a feature is a value: name what to return before the column to rank by: `argMax(transactions as t, t.merchant, t.amount [, cond])`"),
        ("first(transaction)", "`first` here gives the whole event; a feature is a value: name what to return, e.g. `first(transaction as t, t.merchant [, <condition>])`"),
        ("last(transaction as t, t.amount > 100)", "`last` here gives the whole event; a feature is a value: name what to return, e.g. `last(transaction as t, t.merchant [, <condition>])`"),
    ] {
        let found = errors(json!({ "id": "c5", "name": "pick", "type": "object", "feature": { "expr": expr, "window": "7d" } }));
        assert!(found.iter().any(|m| m == message), "{expr}: {found:#?}");
    }
    // A column, or a value and a condition, is fine.
    for expr in ["first(transaction as t, t.merchant)", "last(transaction as t, t.merchant, t.amount > 100)"] {
        let found = errors(json!({ "id": "c5", "name": "pick", "type": "string", "feature": { "expr": expr, "window": "7d" } }));
        assert!(found.iter().all(|m| !m.contains("whole event")), "{expr}: {found:#?}");
    }
    // The approximate and positional aggregates are single values too.
    let found = errors(json!({ "id": "c5", "name": "biggest_merchant", "type": "string", "array": true,
        "feature": { "expr": "argMax(transaction as t, t.merchant, t.amount)", "window": "7d" } }));
    assert!(found.iter().any(|m| m.contains("`biggest_merchant` is a single value (`argMax`)")), "{found:#?}");
    // Grouped: a map, on an `object` property.
    let ok = errors(json!({ "id": "c5", "name": "by_merchant", "type": "object",
        "feature": { "expr": "unique(transaction as t, t.merchant)", "window": "7d" } }));
    assert!(ok.is_empty(), "{ok:#?}");
    // Derived: by its type.
    let found = errors(json!({ "id": "c5", "name": "pair", "type": "number", "feature": { "expr": "[spend_1d, spend_1d]" } }));
    assert!(found.iter().any(|m| m.starts_with("`pair` is a list:")), "{found:#?}");
    let found = errors(json!({ "id": "c5", "name": "half", "type": "number", "array": true, "feature": { "expr": "spend_1d / 2" } }));
    assert!(found.iter().any(|m| m.starts_with("`half` is a single value:")), "{found:#?}");
}

#[test]
fn an_attribute_another_overrides_is_a_warning() {
    let warnings = |doc: serde_json::Value| -> Vec<String> {
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("p", serde_json::from_value(doc).unwrap());
        ws.diagnostics("p")
            .into_iter()
            .filter(|d| d.severity == Severity::Warning)
            .map(|d| d.message)
            .collect()
    };
    let mut doc = fraud("true");
    doc["blocks"][0]["props"]["data"]["reference"] = json!({});
    let found = warnings(doc);
    assert!(found.iter().any(|m| m.contains("`transaction` has both `events` and `reference`")), "{found:#?}");

    let found = warnings(with_call(json!({ "inputs": { "amount": "transaction.amount" } })));
    assert!(found.iter().any(|m| m.contains("call 'risk_score' has both `inputs` and `request`")), "{found:#?}");
    // `"inputs": null` is no inputs: the request is what it sends.
    let doc = with_call(json!({ "inputs": null, "request": "{ amount: amout }" }));
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc.clone()).unwrap());
    assert!(!warnings(doc.clone()).iter().any(|m| m.contains("has both")));
    let request = serde_json::to_value(CursorTarget::ModelRequest { id: Arc::from("t6") }).unwrap();
    assert!(
        ws.diagnostics("p").iter().any(|d| d.location.target.as_ref().map(|t| serde_json::to_value(t).unwrap())
            == Some(request.clone())),
        "the request is checked"
    );
    assert!(ws.facts("p").iter().any(|f| serde_json::to_value(&f.target).unwrap() == request));
}

#[test]
fn a_feature_default_is_typed_and_written_once() {
    let mut doc = fraud("true");
    doc["blocks"][1]["props"]["data"]["properties"][2]["feature"]["default"] = json!(null);
    let parsed: zen_engine::policy::PolicyDocument = serde_json::from_value(doc.clone()).unwrap();
    let text = serde_json::to_string(&parsed).unwrap();
    assert_eq!(text.matches("\"default\":0").count(), 1, "{text}");
    assert_eq!(text.matches("\"default\":null").count(), 1, "kept as written: {text}");
    let back: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(back, serde_json::to_value(&parsed).unwrap());
    assert_eq!(back["blocks"][1]["props"]["data"]["properties"][3]["feature"]["default"], json!(0));
    // Read as the feature's default: never null.
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let card = ws.entities(&ScopeRequest::for_policy("p")).into_iter().find(|e| e.name.as_ref() == "card").unwrap();
    let spend = serde_json::to_value(card.fields.iter().find(|f| f.name.as_ref() == "spend_1d").unwrap()).unwrap();
    assert_eq!(spend["origin"]["supply"]["default"], json!(0), "{spend}");
}

#[test]
fn defaults_fill_pool_items_and_nested_relationships() {
    let doc = json!({ "blocks": [
        { "id": "dm-booking", "type": "dataModel", "props": { "data": {
            "name": "booking",
            "properties": [
                { "id": "b1", "name": "hotel", "type": "reference", "target": "hotel" },
                { "id": "b2", "name": "rooms", "type": "relationship", "target": "room", "array": true }
            ] } } },
        { "id": "dm-hotel", "type": "dataModel", "props": { "data": {
            "name": "hotel",
            "properties": [
                { "id": "h1", "name": "id", "type": "string" },
                { "id": "h2", "name": "stars", "type": "number", "default": 3 },
                { "id": "h3", "name": "opened", "type": "date", "default": "2020-06-01" }
            ] } } },
        { "id": "dm-room", "type": "dataModel", "props": { "data": {
            "name": "room",
            "properties": [
                { "id": "r1", "name": "kind", "type": "string", "default": "double" },
                { "id": "r2", "name": "starts", "type": "date", "default": "2026-03-01" }
            ] } } },
        { "id": "e1", "type": "expression", "props": { "data": {
            "key": "booking.hotel_label", "value": "[booking.hotel.stars, booking.hotel.opened.year()]" } } },
        { "id": "e2", "type": "expression", "props": { "data": {
            "key": "booking.kinds", "value": "map(booking.rooms, #.kind)" } } },
        { "id": "e3", "type": "expression", "props": { "data": {
            "key": "booking.months", "value": "map(booking.rooms, #.starts.month())" } } }
    ] });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let errors: Vec<_> = ws.diagnostics("p").into_iter().filter(|d| d.severity == Severity::Error).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let out = evaluate(
        &ws,
        json!({ "booking": { "hotel": "h1", "rooms": [{ "kind": null }, { "kind": "suite" }] }, "hotel": [{ "id": "h1" }] }),
    )
    .expect("evaluates");
    assert_eq!(out["booking"]["hotel_label"], json!([3, 2020]), "{out}");
    assert_eq!(out["booking"]["kinds"], json!(["double", "suite"]), "{out}");
    assert_eq!(out["booking"]["months"], json!([3, 3]), "{out}");
    let out = evaluate(
        &ws,
        json!({ "booking": { "hotel": "h1", "rooms": [{ "kind": "twin" }, { "starts": "2026-07-04" }] },
                "hotel": [{ "id": "h1", "stars": 5, "opened": "1999-01-01" }] }),
    )
    .expect("evaluates");
    assert_eq!(out["booking"]["hotel_label"], json!([5, 1999]), "{out}");
    assert_eq!(out["booking"]["kinds"], json!(["twin", "double"]), "{out}");
    assert_eq!(out["booking"]["months"], json!([3, 7]), "{out}");
}

#[test]
fn only_the_entities_a_policy_touches_are_its_inputs() {
    // An imported model declares `card_merchant` (a root: nothing references
    // it); the policy only reads `transaction`, so only that is asked for.
    let models = json!({ "blocks": [
        { "id": "t", "type": "dataModel", "props": { "data": {
            "name": "transaction",
            "properties": [{ "id": "t1", "name": "amount", "type": "number" }] } } },
        { "id": "cm", "type": "dataModel", "props": { "data": {
            "name": "card_merchant", "key": ["card", "merchant"],
            "properties": [
                { "id": "c1", "name": "card", "type": "string" },
                { "id": "c2", "name": "merchant", "type": "string" }
            ] } } }
    ] });
    let policy = |value: &str| {
        json!({ "imports": ["models"], "blocks": [
            { "id": "e", "type": "expression", "props": { "data": {
                "key": "transaction.big", "value": value } } }
        ] })
    };
    let paths = |value: &str| {
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("models", serde_json::from_value(models.clone()).unwrap());
        ws.set_policy("p", serde_json::from_value(policy(value)).unwrap());
        let mut paths: Vec<String> =
            ws.inputs(&ScopeRequest::for_policy("p")).into_iter().map(|p| p.path.to_string()).collect();
        paths.sort();
        paths
    };
    assert_eq!(paths("transaction.amount > 9000"), vec!["transaction.amount"]);
    // A read it can't follow: every root entity, as before.
    let all = paths("$ != null");
    assert!(all.iter().any(|p| p.starts_with("card_merchant")), "{all:?}");
}


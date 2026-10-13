//! Outcomes on data model properties: what happened after a decision (a
//! chargeback, how a claim ended), computed offline by the host. An
//! outcome is a feature expression that also reads `decidedAt`, or without
//! `expr` the entity's own column (a column outcome); it is never in a
//! request, and nothing that runs at the decision (a rule, a
//! graph node, a feature, a compute, a call) may read it.

use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::{DecisionContent, PolicyContent};
use zen_engine::policy::{
    Cursor, CursorTarget, DiagnosticCode, EvaluateRequest, PolicyDocument, PolicyWorkspace,
    SchemaAudience, ScopeRequest, Severity,
};
use zen_engine::DecisionEngine;
use zen_expression::variable::{Variable, VariableType};

fn data_model(id: &str, name: &str, extra: Value, properties: Value) -> Value {
    let mut data = json!({ "name": name, "properties": properties });
    data.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({ "id": id, "type": "dataModel", "props": { "data": data } })
}

/// Transactions of a customer, their chargebacks and the customer's claims.
/// The transaction has outcomes: `fraud` (from its chargebacks), `bad`
/// (derived from another outcome), `claims_after` (the customer's claims
/// in the year after the decision) and `heavy` (from a feature).
fn models() -> Vec<Value> {
    vec![
        data_model(
            "dm-customer",
            "customer",
            json!({ "reference": {} }),
            json!([
                { "id": "u1", "name": "id", "type": "string" },
                { "id": "u2", "name": "claims_1825d", "type": "number",
                  "feature": { "expr": "count(claim)", "window": "1825d", "default": 0 } },
                { "id": "u3", "name": "churned", "type": "boolean",
                  "outcome": { "expr": "count(claim as c, c.status == 'closed') > 3" } }
            ]),
        ),
        data_model(
            "dm-transaction",
            "transaction",
            json!({ "key": "txn_id" }),
            json!([
                { "id": "t1", "name": "txn_id", "type": "string" },
                { "id": "t2", "name": "amount", "type": "number" },
                { "id": "t3", "name": "customer", "type": "reference", "target": "customer" },
                { "id": "t4", "name": "fraud", "type": "boolean",
                  "outcome": { "expr": "count(chargeback as c, c.status != 'won') > 0", "maturity": "30d" } },
                { "id": "t5", "name": "bad", "type": "boolean",
                  "outcome": { "expr": "(fraud ?? false) or amount > 1000" } },
                { "id": "t6", "name": "claims_after", "type": "number",
                  "outcome": { "expr": "count(customer.claim as c, d(c.submitted_at) <= d(decidedAt).add(365, 'day'))" } },
                { "id": "t7", "name": "heavy", "type": "boolean",
                  "outcome": { "expr": "customer.claims_1825d > 2 and (fraud ?? false)" } }
            ]),
        ),
        data_model(
            "dm-chargeback",
            "chargeback",
            json!({ "events": { "id": "cb_id", "time": "at" } }),
            json!([
                { "id": "c1", "name": "cb_id", "type": "string" },
                { "id": "c2", "name": "at", "type": "date" },
                { "id": "c3", "name": "transaction", "type": "reference", "target": "transaction" },
                { "id": "c4", "name": "status", "type": "string" }
            ]),
        ),
        data_model(
            "dm-claim",
            "claim",
            json!({ "events": { "id": "claim_id", "time": "submitted_at" } }),
            json!([
                { "id": "k1", "name": "claim_id", "type": "string" },
                { "id": "k2", "name": "submitted_at", "type": "date" },
                { "id": "k3", "name": "customer", "type": "reference", "target": "customer" },
                { "id": "k4", "name": "status", "type": "string" }
            ]),
        ),
    ]
}

fn rule(expr: &str) -> Value {
    json!({ "id": "rule", "type": "expression", "props": { "data": {
        "key": "transaction.big", "value": expr
    } } })
}

fn policy(rule_expr: &str) -> Value {
    let mut blocks = models();
    blocks.push(rule(rule_expr));
    json!({ "blocks": blocks })
}

/// The policy with a transaction property changed or added (by id).
fn with_transaction_prop(mut doc: Value, prop: Value) -> Value {
    let props = doc["blocks"][1]["props"]["data"]["properties"]
        .as_array_mut()
        .unwrap();
    match props.iter().position(|p| p["id"] == prop["id"]) {
        Some(at) => props[at] = prop,
        None => props.push(prop),
    }
    doc
}

fn workspace(doc: Value) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws
}

fn errors(ws: &PolicyWorkspace) -> Vec<(DiagnosticCode, String)> {
    ws.diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| (d.code, d.message))
        .collect()
}

fn cursor(block: &str, pos: usize, target: CursorTarget) -> Cursor {
    Cursor {
        policy_path: Arc::from("p"),
        block_id: Arc::from(block),
        pos: pos as u32,
        target,
    }
}

fn outcome(id: &str) -> CursorTarget {
    CursorTarget::OutcomeExpr { id: Arc::from(id) }
}

fn policy_content(doc: Value) -> DecisionContent {
    DecisionContent::Policy(PolicyContent(Arc::new(
        serde_json::from_value(doc).unwrap(),
    )))
}

#[test]
fn outcomes_read_fields_features_other_outcomes_and_decided_at() {
    let ws = workspace(policy("transaction.amount > 100"));
    let found = errors(&ws);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn an_outcome_round_trips_once() {
    let doc = with_transaction_prop(
        policy("true"),
        json!({ "id": "t4", "name": "fraud", "type": "boolean", "description": "charged back",
            "outcome": { "expr": "count(chargeback) > 0", "maturity": "30d", "template": "happened" } }),
    );
    let parsed: PolicyDocument = serde_json::from_value(doc).unwrap();
    let text = serde_json::to_string(&parsed).unwrap();
    let out: Value = serde_json::from_str(&text).unwrap();
    let written =
        serde_json::to_string(&out["blocks"][1]["props"]["data"]["properties"][3]).unwrap();
    assert_eq!(written.matches("\"outcome\"").count(), 1, "{written}");
    assert_eq!(written.matches("\"maturity\"").count(), 1, "{written}");
    assert_eq!(text.matches("\"template\"").count(), 1, "{text}");
    let prop = &out["blocks"][1]["props"]["data"]["properties"][3];
    assert_eq!(
        prop["outcome"],
        json!({ "expr": "count(chargeback) > 0", "maturity": "30d", "template": "happened" })
    );
    assert_eq!(prop["description"], "charged back");
    let zen_engine::policy::BlockDoc::DataModel { data, .. } = &parsed.blocks[1] else {
        panic!("a data model");
    };
    assert!(data.properties[3].rest.get("outcome").is_none());
    assert!(data.properties[3]
        .outcome
        .as_ref()
        .unwrap()
        .rest
        .contains_key("template"));
    // Reads back the same.
    let again: PolicyDocument = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_value(&again).unwrap(), out);
}

/// The policy with `chargebacks` (a column outcome: the transaction's own
/// column, read by the host) and `lost`, an outcome reading it.
fn with_column_outcome(rule_expr: &str) -> Value {
    let doc = with_transaction_prop(
        policy(rule_expr),
        json!({ "id": "t8", "name": "chargebacks", "type": "boolean",
            "column": "cb_flag", "outcome": { "maturity": "30d" } }),
    );
    with_transaction_prop(
        doc,
        json!({ "id": "t9", "name": "lost", "type": "boolean",
            "outcome": { "expr": "(chargebacks ?? false) and amount > 100" } }),
    )
}

#[test]
fn a_column_outcome_is_valid_and_never_a_feature_too() {
    // Without `expr` (absent or blank): the entity's own column.
    for outcome in [
        json!({}),
        json!({ "maturity": "30d" }),
        json!({ "expr": "  " }),
    ] {
        let ws = workspace(with_transaction_prop(
            policy("true"),
            json!({ "id": "t4", "name": "fraud", "type": "boolean", "outcome": outcome }),
        ));
        let found = errors(&ws);
        assert!(found.is_empty(), "{outcome}: {found:#?}");
    }
    // Typed by its `type`, read by other outcomes.
    let ws = workspace(with_column_outcome("true"));
    let found = errors(&ws);
    assert!(found.is_empty(), "{found:#?}");
    // The editor shows an empty expression line: scope as any outcome's.
    let labels: Vec<String> = ws
        .completions(&cursor("dm-transaction", 0, outcome("t8")))
        .into_iter()
        .map(|c| c.label)
        .collect();
    for expected in ["decidedAt", "amount", "chargeback"] {
        assert!(
            labels.iter().any(|l| l == expected),
            "{expected} in {labels:?}"
        );
    }

    let ws = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t4", "name": "fraud", "type": "boolean",
            "feature": { "expr": "count(chargeback) > 0" },
            "outcome": { "expr": "count(chargeback) > 0" } }),
    ));
    let found = errors(&ws);
    assert!(
        found
            .iter()
            .any(|(code, m)| *code == DiagnosticCode::InvalidOutcome
                && m.contains("has both `feature` and `outcome`")),
        "{found:#?}"
    );
}

#[test]
fn maturity_is_a_number_of_days() {
    let check = |maturity: Value| {
        let ws = workspace(with_transaction_prop(
            policy("true"),
            json!({ "id": "t4", "name": "fraud", "type": "boolean",
                "outcome": { "expr": "count(chargeback) > 0", "maturity": maturity } }),
        ));
        errors(&ws)
            .into_iter()
            .filter(|(code, _)| *code == DiagnosticCode::InvalidDuration)
            .map(|(_, m)| m)
            .collect::<Vec<_>>()
    };
    for ok in [json!("30d"), json!("120d"), json!("365d"), Value::Null] {
        assert!(check(ok.clone()).is_empty(), "{ok}");
    }
    for bad in [
        json!("1M"),
        json!("5m"),
        json!("05d"),
        json!(" 30d"),
        json!("30d "),
        json!("0d"),
        json!("d"),
        json!(30),
        json!(["30d"]),
    ] {
        let found = check(bad.clone());
        assert_eq!(found.len(), 1, "{bad}: {found:#?}");
        assert!(
            found[0].starts_with(
                "`maturity` of outcome 'fraud' in entity 'transaction' is a number of days"
            ),
            "{found:#?}"
        );
    }
    assert!(check(json!("1M"))[0].ends_with("not `1M`"));
    assert!(check(json!(["30d"]))[0].ends_with("not `[\"30d\"]`"));
}

#[test]
fn unknown_for_is_now_maturity() {
    let ws = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t4", "name": "fraud", "type": "boolean",
            "outcome": { "expr": "count(chargeback) > 0", "unknownFor": "30d" } }),
    ));
    assert!(errors(&ws).is_empty(), "{:#?}", errors(&ws));
    let warnings: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Warning)
        .map(|d| (d.code, d.message))
        .collect();
    assert_eq!(
        warnings,
        vec![(
            DiagnosticCode::InvalidOutcome,
            "`unknownFor` is now `maturity` in outcome 'fraud' in entity 'transaction'".to_string()
        )]
    );
}

#[test]
fn decided_at_is_a_date_in_outcomes_only() {
    let ws = workspace(policy("true"));
    let source = "count(customer.claim as c, d(c.submitted_at) <= d(decidedAt).add(365, 'day'))";
    let at = source.find("decidedAt").unwrap() + 2;
    let hover = ws
        .inspect(&cursor("dm-transaction", at, outcome("t6")))
        .expect("hover on decidedAt");
    assert!(matches!(hover.kind, VariableType::Date), "{hover:#?}");
    let labels: Vec<String> = ws
        .completions(&cursor("dm-transaction", 0, outcome("t5")))
        .into_iter()
        .map(|c| c.label)
        .collect();
    for expected in [
        "decidedAt",
        "amount",
        "fraud",
        "customer",
        "chargeback",
        "params",
    ] {
        assert!(
            labels.iter().any(|l| l == expected),
            "{expected} in {labels:?}"
        );
    }

    // A feature is known at the decision: no `decidedAt` there.
    let ws = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "age", "type": "number",
            "feature": { "expr": "d(asOf).diff(d(decidedAt), 'day')" } }),
    ));
    let found = errors(&ws);
    assert!(
        found.iter().any(|(_, m)| m.contains("decidedAt")),
        "{found:#?}"
    );
    let labels: Vec<String> = ws
        .completions(&cursor(
            "dm-transaction",
            0,
            CursorTarget::FeatureExpr {
                id: Arc::from("t8"),
            },
        ))
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(!labels.iter().any(|l| l == "decidedAt"), "{labels:?}");
}

#[test]
fn an_outcome_reads_records_through_a_reference() {
    // `customer.claim`: the claims whose `customer` is this transaction's.
    let ws = workspace(policy("true"));
    let source = "count(customer.claim as c, d(c.";
    let ws_typing = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t6", "name": "claims_after", "type": "number", "outcome": { "expr": source } }),
    ));
    let members: Vec<String> = ws_typing
        .completions(&cursor("dm-transaction", source.len(), outcome("t6")))
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(members.iter().any(|l| l == "submitted_at"), "{members:?}");
    // Not a field of the customer as rules see it.
    let found = errors(&workspace(policy("count(transaction.customer.claim) > 0")));
    assert!(!found.is_empty(), "{found:#?}");
    assert!(errors(&ws).is_empty());
}

#[test]
fn an_outcome_is_typed_against_its_property() {
    let ws = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t4", "name": "fraud", "type": "number",
            "outcome": { "expr": "count(chargeback as c, c.status != 'won') > 0" } }),
    ));
    let found = errors(&ws);
    assert!(
        found
            .iter()
            .any(|(code, m)| *code == DiagnosticCode::TypeMismatch
                && m == "outcome `fraud` gives a boolean, but its `type` is number"),
        "{found:#?}"
    );
    let ws = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t9", "name": "status", "type": "string",
            "outcome": { "expr": "last(chargeback as c, c.status)" } }),
    ));
    assert!(errors(&ws).is_empty(), "{:#?}", errors(&ws));
    let ws = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t9", "name": "status", "type": "boolean",
            "outcome": { "expr": "last(chargeback as c, c.status)" } }),
    ));
    assert!(
        errors(&ws)
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::TypeMismatch),
        "{:#?}",
        errors(&ws)
    );
}

#[test]
fn an_outcome_is_a_value_of_its_shape() {
    let shape = |prop: Value| {
        errors(&workspace(with_transaction_prop(policy("true"), prop)))
            .into_iter()
            .filter(|(code, _)| *code == DiagnosticCode::FeatureShape)
            .map(|(_, m)| m)
            .collect::<Vec<_>>()
    };
    let found = shape(json!({ "id": "t9", "name": "statuses", "type": "string",
        "outcome": { "expr": "unique(chargeback as c, c.status)" } }));
    assert_eq!(found, vec!["`statuses` is a list (`unique`): set `array: true` on the property (its `type` is the items')".to_string()]);
    assert!(shape(
        json!({ "id": "t9", "name": "statuses", "type": "string", "array": true,
        "outcome": { "expr": "unique(chargeback as c, c.status)" } })
    )
    .is_empty());
    let found = shape(json!({ "id": "t9", "name": "first_cb", "type": "object",
        "outcome": { "expr": "first(chargeback)" } }));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].contains("gives the whole event; an outcome is a value"),
        "{found:#?}"
    );
}

#[test]
fn an_outcome_reading_itself_is_a_cycle() {
    let doc = with_transaction_prop(
        policy("true"),
        json!({ "id": "t5", "name": "bad", "type": "boolean", "outcome": { "expr": "worse or fraud" } }),
    );
    let doc = with_transaction_prop(
        doc,
        json!({ "id": "t8", "name": "worse", "type": "boolean", "outcome": { "expr": "bad and amount > 5" } }),
    );
    let found = errors(&workspace(doc));
    assert!(
        found
            .iter()
            .any(|(code, m)| *code == DiagnosticCode::CyclicDependency
                && m.contains("`bad` depends on itself: bad → worse → bad")),
        "{found:#?}"
    );
}

fn outcome_reads(ws: &PolicyWorkspace) -> Vec<String> {
    ws.diagnostics("p")
        .into_iter()
        .filter(|d| d.code == DiagnosticCode::OutcomeRead)
        .inspect(|d| assert_eq!(d.severity, Severity::Error))
        .map(|d| d.message)
        .collect()
}

#[test]
fn rules_cannot_read_an_outcome() {
    let ws = workspace(policy("transaction.fraud == true"));
    assert_eq!(
        outcome_reads(&ws),
        vec!["`transaction.fraud` is an outcome: known only after the decision; rules cannot read it"]
    );
    let found = ws.diagnostics("p");
    let read = found
        .iter()
        .find(|d| d.code == DiagnosticCode::OutcomeRead)
        .unwrap();
    assert_eq!(read.location.block_id.as_deref(), Some("rule"));
    assert!(read.location.span.is_some());

    // Through a reference.
    let ws = workspace(policy("transaction.customer.churned ?? false"));
    assert_eq!(
        outcome_reads(&ws),
        vec![
            "`customer.churned` is an outcome: known only after the decision; rules cannot read it"
        ]
    );

    // In a decision table's column.
    let mut doc = policy("true");
    doc["blocks"].as_array_mut().unwrap().push(
        json!({ "id": "dt", "type": "decisionTable", "props": { "data": {
        "hitPolicy": "first",
        "inputs": [ { "id": "i0", "name": "Bad", "field": "transaction.bad" } ],
        "outputs": [ { "id": "o0", "name": "Flag", "field": "transaction.flag" } ],
        "rules": [ { "_id": "r1", "i0": "true", "o0": "1" } ]
    } } }),
    );
    assert_eq!(
        outcome_reads(&workspace(doc)),
        vec![
            "`transaction.bad` is an outcome: known only after the decision; rules cannot read it"
        ]
    );

    // Fields and features are fine.
    assert!(outcome_reads(&workspace(policy(
        "transaction.amount > transaction.customer.claims_1825d"
    )))
    .is_empty());
}

#[test]
fn features_computes_and_calls_cannot_read_an_outcome() {
    let derived = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "flagged", "type": "boolean", "feature": { "expr": "fraud ?? false" } }),
    ));
    assert_eq!(
        outcome_reads(&derived),
        vec!["`transaction.fraud` is an outcome: known only after the decision; features cannot read it"]
    );
    let read = derived
        .diagnostics("p")
        .into_iter()
        .find(|d| d.code == DiagnosticCode::OutcomeRead)
        .unwrap();
    assert_eq!(
        serde_json::to_value(read.location.target.as_ref().unwrap()).unwrap(),
        json!({ "kind": "featureExpr", "id": "t8" })
    );

    // A windowed feature aggregating it.
    let mut doc = policy("true");
    doc["blocks"][0]["props"]["data"]["properties"]
        .as_array_mut()
        .unwrap()
        .push(json!({
        "id": "u4", "name": "frauds", "type": "number",
        "feature": { "expr": "count(transaction as t, t.fraud)", "window": "30d" } }));
    assert_eq!(
        outcome_reads(&workspace(doc)),
        vec!["`transaction.fraud` is an outcome: known only after the decision; features cannot read it"]
    );

    let compute = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "scaled", "type": "number", "compute": "(claims_after ?? 0) * 2" }),
    ));
    assert_eq!(
        outcome_reads(&compute),
        vec!["`transaction.claims_after` is an outcome: known only after the decision; features cannot read it"]
    );

    let call = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "score", "type": "number",
            "model": { "datasource": "fraud", "request": "{ amount, fraud }", "when": "bad != true" } }),
    ));
    assert_eq!(
        outcome_reads(&call),
        vec![
            "`transaction.bad` is an outcome: known only after the decision; calls cannot read it",
            "`transaction.fraud` is an outcome: known only after the decision; calls cannot read it",
        ]
    );
}

#[test]
fn a_read_of_an_outcome_stops_evaluation_and_compile() {
    let ws = workspace(policy("transaction.fraud == true"));
    let result = ws.evaluate(&EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(
            json!({ "transaction": { "txn_id": "T1", "amount": 5, "customer": "C1" } }),
        ),
        goals: Vec::new(),
        trace: false,
    });
    assert!(result.is_err(), "{:?}", result.map(|r| r.output));

    let loader = Arc::new(MemoryLoader::default());
    loader.add("p", policy_content(policy("transaction.fraud == true")));
    loader.add("ok", policy_content(policy("transaction.amount > 5")));
    let derived = with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "flagged", "type": "boolean", "feature": { "expr": "fraud ?? false" } }),
    );
    loader.add("feature", policy_content(derived));
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    let failed = |key: &str, message: &str| {
        failures.iter().any(|f| {
            f.key.as_ref() == key && f.diagnostics.iter().any(|d| d.message.contains(message))
        })
    };
    assert!(failed("p", "rules cannot read it"), "{failures:#?}");
    assert!(
        failed("feature", "features cannot read it"),
        "{failures:#?}"
    );
    assert!(
        !failures.iter().any(|f| f.key.as_ref() == "ok"),
        "{failures:#?}"
    );
}

fn graph(expression: &str) -> Value {
    json!({
        "imports": ["models"],
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": { "target": "transaction" } },
            { "id": "ex", "name": "ex", "type": "expressionNode", "content": { "expressions": [
                { "id": "x0", "key": "flag", "value": expression }
            ] } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "ex" },
            { "id": "e2", "sourceId": "ex", "targetId": "out" }
        ]
    })
}

fn models_document() -> Value {
    json!({ "blocks": models() })
}

#[test]
fn graph_nodes_cannot_read_an_outcome() {
    let mut ws = PolicyWorkspace::new();
    ws.set_document("models", serde_json::from_value(models_document()).unwrap());
    ws.set_document(
        "g",
        serde_json::from_value(graph("fraud == true or customer.churned == true")).unwrap(),
    );
    let found: Vec<_> = ws
        .diagnostics("g")
        .into_iter()
        .filter(|d| d.code == DiagnosticCode::OutcomeRead)
        .collect();
    let messages: Vec<&str> = found.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        vec![
            "`transaction.fraud` is an outcome: known only after the decision; rules cannot read it",
            "`customer.churned` is an outcome: known only after the decision; rules cannot read it",
        ]
    );
    assert!(found
        .iter()
        .all(|d| d.severity == Severity::Error && d.location.block_id.as_deref() == Some("ex")));

    ws.set_document("g", serde_json::from_value(graph("amount > 5")).unwrap());
    assert!(!ws
        .diagnostics("g")
        .iter()
        .any(|d| d.code == DiagnosticCode::OutcomeRead));

    // The graph's request leaves outcomes out.
    let schema = ws.request_schema(&ScopeRequest::for_policy("g"), SchemaAudience::Evaluated);
    assert!(schema["properties"].get("amount").is_some(), "{schema:#}");
    assert!(schema["properties"].get("fraud").is_none(), "{schema:#}");

    // Compiling fails the graph.
    let loader = Arc::new(MemoryLoader::default());
    loader.add(
        "models",
        serde_json::from_value::<DecisionContent>(models_document()).unwrap(),
    );
    loader.add(
        "g",
        serde_json::from_value::<DecisionContent>(graph("fraud == true")).unwrap(),
    );
    loader.add(
        "fine",
        serde_json::from_value::<DecisionContent>(graph("amount > 5")).unwrap(),
    );
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(
        failures.iter().any(|f| f.key.as_ref() == "g"
            && f.kind == "graph"
            && f.diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::OutcomeRead)),
        "{failures:#?}"
    );
    assert!(
        !failures.iter().any(|f| f.key.as_ref() == "fine"),
        "{failures:#?}"
    );
}

#[test]
fn requests_leave_outcomes_out() {
    let ws = workspace(policy(
        "transaction.amount > transaction.customer.claims_1825d",
    ));
    let req = ScopeRequest::for_policy("p");
    let inputs = serde_json::to_string(&ws.inputs(&req)).unwrap();
    assert!(inputs.contains("\"amount\""), "{inputs}");
    let outcomes = [
        "\"fraud\"",
        "\"bad\"",
        "\"claims_after\"",
        "\"heavy\"",
        "\"churned\"",
    ];
    for outcome in outcomes {
        assert!(!inputs.contains(outcome), "{outcome} in {inputs}");
    }
    for audience in [SchemaAudience::Contract, SchemaAudience::Evaluated] {
        let schema = ws.request_schema(&req, audience).to_string();
        assert!(schema.contains("\"amount\""), "{schema}");
        for outcome in outcomes {
            assert!(!schema.contains(outcome), "{outcome} in {schema}");
        }
    }
    // The response is what the rules write, as before.
    let response = ws.response_schema(&req).to_string();
    assert!(response.contains("\"big\""), "{response}");
}

#[test]
fn entities_mark_an_outcome_as_supplied_after_the_decision() {
    let ws = workspace(policy("true"));
    let entities = serde_json::to_value(ws.entities(&ScopeRequest::for_policy("p"))).unwrap();
    let field = |entity: &str, name: &str| {
        entities
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == entity)
            .and_then(|e| {
                e["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|f| f["name"] == name)
            })
            .unwrap_or_else(|| panic!("{entity}.{name}"))
            .clone()
    };
    assert_eq!(
        field("transaction", "fraud")["origin"]["supply"],
        json!({ "kind": "outcome", "expr": "count(chargeback as c, c.status != 'won') > 0", "maturity": "30d" })
    );
    assert_eq!(
        field("transaction", "claims_after")["origin"]["supply"],
        json!({ "kind": "outcome", "expr": "count(customer.claim as c, d(c.submitted_at) <= d(decidedAt).add(365, 'day'))" })
    );
    // Host-supplied and nullable.
    let ws = workspace(policy("transaction.fraud"));
    let hover = ws
        .inspect(&cursor(
            "rule",
            "transaction.fr".len(),
            CursorTarget::Expression {
                id: Arc::from("s1"),
            },
        ))
        .expect("hover");
    assert_eq!(
        hover.detail.as_deref(),
        Some("bool? · outcome count(chargeback as c, c.status != 'won') > 0 · mature after 30d")
    );
}

#[test]
fn a_column_outcome_round_trips_without_expr() {
    for (written, expected) in [
        (json!({}), json!({})),
        (json!({ "maturity": "30d" }), json!({ "maturity": "30d" })),
        // Blank is no expression: written back without it.
        (
            json!({ "expr": "", "maturity": "30d" }),
            json!({ "maturity": "30d" }),
        ),
        (json!({ "expr": null }), json!({})),
    ] {
        let doc = with_transaction_prop(
            policy("true"),
            json!({ "id": "t4", "name": "fraud", "type": "boolean", "outcome": written }),
        );
        let parsed: PolicyDocument = serde_json::from_value(doc).unwrap();
        let out = serde_json::to_value(&parsed).unwrap();
        let prop = &out["blocks"][1]["props"]["data"]["properties"][3];
        assert_eq!(prop["outcome"], expected, "{written}");
        let again: PolicyDocument = serde_json::from_value(out.clone()).unwrap();
        assert_eq!(serde_json::to_value(&again).unwrap(), out);
    }
}

#[test]
fn rules_cannot_read_a_column_outcome() {
    let ws = workspace(with_column_outcome("transaction.chargebacks == true"));
    assert_eq!(
        outcome_reads(&ws),
        vec!["`transaction.chargebacks` is an outcome: known only after the decision; rules cannot read it"]
    );
    // Nor can a feature.
    let ws = workspace(with_transaction_prop(
        with_column_outcome("true"),
        json!({ "id": "t10", "name": "flagged", "type": "boolean",
            "feature": { "expr": "chargebacks ?? false" } }),
    ));
    assert!(
        errors(&ws)
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::OutcomeRead),
        "{:#?}",
        errors(&ws)
    );
}

#[test]
fn a_column_outcome_is_supplied_without_expr() {
    let ws = workspace(with_column_outcome("transaction.chargebacks"));
    let entities = serde_json::to_value(ws.entities(&ScopeRequest::for_policy("p"))).unwrap();
    let transaction = entities
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "transaction")
        .unwrap();
    let field = transaction["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "chargebacks")
        .unwrap();
    assert_eq!(
        field["origin"]["supply"],
        json!({ "kind": "outcome", "maturity": "30d" })
    );
    let hover = ws
        .inspect(&cursor(
            "rule",
            "transaction.ch".len(),
            CursorTarget::Expression {
                id: Arc::from("s1"),
            },
        ))
        .expect("hover");
    assert_eq!(
        hover.detail.as_deref(),
        Some("bool? · outcome from its column · mature after 30d")
    );
}

/// `label`: true when fraud, false once a "no" can be trusted (`fraud`
/// is mature), else unknown.
const LABEL: &str = "fraud == true ? true : (fraud_mature ? false : null)";

fn with_label(doc: Value, expr: &str) -> Value {
    with_transaction_prop(
        doc,
        json!({ "id": "t8", "name": "label", "type": "boolean", "outcome": { "expr": expr } }),
    )
}

#[test]
fn an_outcome_reads_the_maturity_flag_of_an_outcome_with_maturity() {
    let ws = workspace(with_label(policy("true"), LABEL));
    let found = errors(&ws);
    assert!(found.is_empty(), "{found:#?}");

    // Offered, with what it is; only for outcomes with `maturity`.
    let completions = ws.completions(&cursor("dm-transaction", 0, outcome("t8")));
    let flag = completions
        .iter()
        .find(|c| c.label == "fraud_mature")
        .unwrap_or_else(|| panic!("fraud_mature in {completions:#?}"));
    assert_eq!(flag.detail, "bool · whether `fraud` is mature (after 30d)");
    assert!(!completions.iter().any(|c| c.label == "bad_mature"));
    let at = LABEL.find("fraud_mature").unwrap() + 3;
    let hover = ws
        .inspect(&cursor("dm-transaction", at, outcome("t8")))
        .expect("hover on fraud_mature");
    assert!(matches!(hover.kind, VariableType::Bool), "{hover:#?}");
    assert_eq!(
        hover.detail.as_deref(),
        Some("bool · whether `fraud` is mature (after 30d)")
    );

    // Not in a feature's scope.
    let ws = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "flagged", "type": "boolean", "feature": { "expr": "" } }),
    ));
    assert!(!ws
        .completions(&cursor(
            "dm-transaction",
            0,
            CursorTarget::FeatureExpr {
                id: Arc::from("t8"),
            },
        ))
        .iter()
        .any(|c| c.label == "fraud_mature"));
}

#[test]
fn a_maturity_flag_needs_its_outcome_to_declare_maturity() {
    // `bad` declares no maturity.
    let expr = "bad_mature ? bad : null";
    let ws = workspace(with_label(policy("true"), expr));
    assert_eq!(
        errors(&ws),
        vec![(
            DiagnosticCode::InvalidOutcome,
            "`bad` declares no maturity".to_string()
        )]
    );
    let read = ws
        .diagnostics("p")
        .into_iter()
        .find(|d| d.code == DiagnosticCode::InvalidOutcome)
        .unwrap();
    assert_eq!(read.location.span, Some((0, "bad_mature".len() as u32)));

    // Not an outcome at all: an unknown name, as any.
    let ws = workspace(with_label(policy("true"), "amount_mature ? true : null"));
    let found = errors(&ws);
    assert!(!found.is_empty(), "{found:#?}");
    assert!(
        !found
            .iter()
            .any(|(code, _)| *code == DiagnosticCode::InvalidOutcome),
        "{found:#?}"
    );
}

#[test]
fn nothing_at_the_decision_reads_a_maturity_flag() {
    let rules = workspace(with_label(
        policy("transaction.fraud_mature == true"),
        LABEL,
    ));
    assert_eq!(
        errors(&rules),
        vec![(
            DiagnosticCode::OutcomeRead,
            "`transaction.fraud_mature` is an outcome: known only after the decision; rules cannot read it".to_string()
        )]
    );

    let feature = workspace(with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "flagged", "type": "boolean", "feature": { "expr": "fraud_mature" } }),
    ));
    assert_eq!(
        errors(&feature),
        vec![(
            DiagnosticCode::OutcomeRead,
            "`transaction.fraud_mature` is an outcome: known only after the decision; features cannot read it".to_string()
        )]
    );

    // Through a reference, and through `$root`.
    let mut doc = policy("true");
    doc["blocks"][0]["props"]["data"]["properties"]
        .as_array_mut()
        .unwrap()
        .push(
            json!({ "id": "u4", "name": "churn_known", "type": "boolean",
            "outcome": { "expr": "count(claim) > 0", "maturity": "90d" } }),
        );
    let doc = with_transaction_prop(
        doc,
        json!({ "id": "t8", "name": "score", "type": "number",
            "model": { "datasource": "fraud",
                "request": "{ amount, mature: fraud_mature, known: customer.churn_known_mature }",
                "response": "$root.transaction.fraud_mature ? 1 : response.score" } }),
    );
    let call = workspace(doc.clone());
    let mut found = outcome_reads(&call);
    found.sort();
    assert_eq!(
        found,
        vec![
            "`customer.churn_known_mature` is an outcome: known only after the decision; calls cannot read it",
            "`transaction.fraud_mature` is an outcome: known only after the decision; calls cannot read it",
            "`transaction.fraud_mature` is an outcome: known only after the decision; calls cannot read it",
        ]
    );
    assert!(
        errors(&call)
            .iter()
            .all(|(code, _)| *code == DiagnosticCode::OutcomeRead),
        "{:#?}",
        errors(&call)
    );

    // A graph node.
    let mut ws = PolicyWorkspace::new();
    ws.set_document("models", serde_json::from_value(models_document()).unwrap());
    ws.set_document(
        "g",
        serde_json::from_value(graph("fraud_mature == true")).unwrap(),
    );
    let found: Vec<(DiagnosticCode, String)> = ws
        .diagnostics("g")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| (d.code, d.message))
        .collect();
    assert_eq!(
        found,
        vec![(
            DiagnosticCode::OutcomeRead,
            "`transaction.fraud_mature` is an outcome: known only after the decision; rules cannot read it".to_string()
        )]
    );
}

#[test]
fn a_maturity_flag_reads_its_outcome() {
    // `fraud` reads `label`, which reads only `fraud_mature`: a cycle.
    let doc = with_label(policy("true"), "fraud_mature ? false : null");
    let doc = with_transaction_prop(
        doc,
        json!({ "id": "t4", "name": "fraud", "type": "boolean",
            "outcome": { "expr": "(label ?? false) or count(chargeback) > 0", "maturity": "30d" } }),
    );
    let found = errors(&workspace(doc));
    assert!(
        found
            .iter()
            .any(|(code, m)| *code == DiagnosticCode::CyclicDependency
                && m.contains("depends on itself")
                && m.contains("label")),
        "{found:#?}"
    );
}

#[test]
fn a_property_named_like_a_maturity_flag_clashes() {
    let doc = with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "fraud_mature", "type": "boolean" }),
    );
    let ws = workspace(doc);
    assert_eq!(
        errors(&ws),
        vec![(
            DiagnosticCode::InvalidOutcome,
            "`fraud_mature` clashes with the maturity flag of outcome `fraud`; rename one"
                .to_string()
        )]
    );
    let clash = ws
        .diagnostics("p")
        .into_iter()
        .find(|d| d.code == DiagnosticCode::InvalidOutcome)
        .unwrap();
    assert_eq!(clash.location.expression_id.as_deref(), Some("t8"));
    // Without `maturity` there is no flag: no clash.
    let doc = with_transaction_prop(
        policy("true"),
        json!({ "id": "t8", "name": "bad_mature", "type": "boolean" }),
    );
    assert!(errors(&workspace(doc)).is_empty());
}

#[test]
fn requests_leave_maturity_flags_out() {
    let ws = workspace(with_label(policy("transaction.amount > 5"), LABEL));
    let req = ScopeRequest::for_policy("p");
    let inputs = serde_json::to_string(&ws.inputs(&req)).unwrap();
    assert!(inputs.contains("\"amount\""), "{inputs}");
    assert!(!inputs.contains("_mature"), "{inputs}");
    for audience in [SchemaAudience::Contract, SchemaAudience::Evaluated] {
        let schema = ws.request_schema(&req, audience).to_string();
        assert!(schema.contains("\"amount\""), "{schema}");
        assert!(!schema.contains("_mature"), "{schema}");
    }
    let entities = serde_json::to_string(&ws.entities(&req)).unwrap();
    assert!(!entities.contains("fraud_mature\""), "{entities}");

    let mut ws = PolicyWorkspace::new();
    ws.set_document("models", serde_json::from_value(models_document()).unwrap());
    ws.set_document("g", serde_json::from_value(graph("amount > 5")).unwrap());
    let schema = ws
        .request_schema(&ScopeRequest::for_policy("g"), SchemaAudience::Evaluated)
        .to_string();
    assert!(!schema.contains("_mature"), "{schema}");
}

fn nullable_conditions(ws: &PolicyWorkspace) -> Vec<(Severity, String)> {
    ws.diagnostics("p")
        .into_iter()
        .filter(|d| d.code == DiagnosticCode::NullableCondition)
        .map(|d| (d.severity, d.message))
        .collect()
}

/// `fraud` (an outcome) is `bool?`: as a condition a null fails offline,
/// so an outcome reading it so is an error.
#[test]
fn an_outcome_using_a_nullable_outcome_as_a_condition_is_an_error() {
    let message = "`fraud` may be null (`bool?`): a null condition fails; write `fraud == true`, `fraud ?? false`, or `fraud == null ? … : …`.";
    for expr in [
        "fraud ? true : (fraud_mature ? false : null)",
        "fraud and fraud_mature",
        "fraud_mature or fraud",
        "not fraud",
        "count(chargeback as c, fraud) > 0",
    ] {
        let ws = workspace(with_label(policy("true"), expr));
        assert_eq!(
            nullable_conditions(&ws),
            vec![(Severity::Error, message.to_string())],
            "{expr}"
        );
        let found = ws
            .diagnostics("p")
            .into_iter()
            .find(|d| d.code == DiagnosticCode::NullableCondition)
            .unwrap();
        let at = expr
            .find("fraud")
            .filter(|_| !expr.starts_with("fraud_mature"));
        let at = at.unwrap_or_else(|| expr.rfind("fraud").unwrap()) as u32;
        assert_eq!(found.location.span, Some((at, at + 5)), "{expr}");
        assert!(matches!(
            found.location.target,
            Some(CursorTarget::OutcomeExpr { .. })
        ));
    }

    // Named as written.
    let ws = workspace(with_label(policy("true"), "(fraud ?? null) ? true : false"));
    assert_eq!(
        nullable_conditions(&ws),
        vec![(
            Severity::Error,
            "`(fraud ?? null)` may be null (`bool?`): a null condition fails; write `(fraud ?? null) == true`, `(fraud ?? null) ?? false`, or `(fraud ?? null) == null ? … : …`.".to_string()
        )]
    );
}

#[test]
fn a_nullable_condition_compared_defaulted_or_narrowed_is_clean() {
    for expr in [
        LABEL,
        "fraud ?? false",
        "(fraud ?? false) ? true : null",
        "fraud == null ? null : fraud",
        "fraud == null ? null : (fraud ? true : false)",
        "fraud != null ? (fraud ? true : false) : null",
        "fraud != null and fraud",
        "fraud == null or not fraud",
        "fraud_mature and fraud != null and (fraud or fraud_mature)",
        // A maturity flag is never null.
        "fraud_mature ? true : null",
        "not fraud_mature",
    ] {
        let ws = workspace(with_label(policy("true"), expr));
        let found = ws.diagnostics("p");
        assert!(found.is_empty(), "{expr}: {found:#?}");
    }
    // Narrowed only where proven: not in the other branch.
    let ws = workspace(with_label(
        policy("true"),
        "fraud != null ? true : (fraud ? true : null)",
    ));
    assert_eq!(nullable_conditions(&ws).len(), 1);
}

/// In a rule (and anything that runs at the decision) the same use is a
/// warning: it fails on null, but what compiled before still compiles.
#[test]
fn a_rule_using_a_nullable_field_as_a_condition_is_an_error() {
    let doc = with_transaction_prop(
        policy("transaction.flag ? 1 : 2"),
        json!({ "id": "t8", "name": "flag", "type": "boolean", "optional": true }),
    );
    let ws = workspace(doc.clone());
    assert_eq!(
        nullable_conditions(&ws),
        vec![(
            Severity::Error,
            "`transaction.flag` may be null (`bool?`): a null condition fails; write `transaction.flag == true`, `transaction.flag ?? false`, or `transaction.flag == null ? … : …`.".to_string()
        )]
    );
    let ws = workspace(with_transaction_prop(
        policy("transaction.flag == true ? 1 : 2"),
        json!({ "id": "t8", "name": "flag", "type": "boolean", "optional": true }),
    ));
    assert!(nullable_conditions(&ws).is_empty());

    let loader = Arc::new(MemoryLoader::default());
    loader.add("p", policy_content(doc));
    let engine = DecisionEngine::default().with_loader(loader);
    // It fails at the decision when null: compiling stops on it, as before.
    assert!(!engine.compile().is_empty());
}

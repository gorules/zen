//! A graph's Request node typed by an entity (`inputNode.content.target`)
//! instead of a schema: the entity is visible through the graph's imports,
//! and the request is that entity, flat; a reference holds its record.

use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{DiagnosticCode, Severity, Workspace};

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn edge(id: &str, source: &str, target: &str) -> Value {
    json!({ "id": id, "sourceId": source, "targetId": target, "sourceHandle": null })
}

fn entity(id: &str, name: &str, properties: Value) -> Value {
    json!({ "id": id, "type": "dataModel", "props": { "data": { "name": name, "properties": properties } } })
}

fn prop(name: &str, ty: &str) -> Value {
    json!({ "id": name, "name": name, "type": ty, "array": false, "optional": false })
}

/// `models/aml`: a transaction referencing its customer; a channel dictionary.
fn models(transaction_fields: Value) -> Value {
    json!({
        "imports": [],
        "blocks": [
            entity("b-txn", "transaction", transaction_fields),
            entity("b-cust", "customer", json!([
                prop("id", "string"),
                { "id": "risk", "name": "risk_rating", "type": "string", "enum": ["low", "high"], "array": false, "optional": false }
            ])),
            { "id": "b-ch", "type": "dictionary", "props": { "data": {
                "name": "channel",
                "entries": [{ "id": "c1", "value": "cash", "label": "Cash" }, { "id": "c2", "value": "wire", "label": "Wire" }]
            } } }
        ]
    })
}

fn transaction_fields() -> Value {
    json!([
        prop("txn_id", "string"),
        prop("amount", "number"),
        { "id": "channel", "name": "channel", "type": "relationship", "target": "channel", "array": false, "optional": false },
        { "id": "customer", "name": "customer", "type": "reference", "target": "customer", "array": false, "optional": false }
    ])
}

fn graph(imports: &[&str], request: Value, expressions: &[(&str, &str)]) -> Value {
    let expressions: Vec<Value> = expressions
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({ "id": format!("x{i}"), "key": key, "value": value }))
        .collect();
    json!({
        "imports": imports,
        "nodes": [
            node("in", "inputNode", request),
            node("ex", "expressionNode", json!({ "expressions": expressions })),
            node("out", "outputNode", json!({}))
        ],
        "edges": [edge("e1", "in", "ex"), edge("e2", "ex", "out")]
    })
}

fn codes(ws: &Workspace, path: &str, severity: Severity) -> Vec<DiagnosticCode> {
    ws.diagnostics(path)
        .into_iter()
        .filter(|d| d.severity == severity)
        .map(|d| d.code)
        .collect()
}

fn workspace(request: Value, expressions: &[(&str, &str)]) -> Workspace {
    let mut ws = Workspace::new();
    ws.set_document("models/aml", document(models(transaction_fields())));
    ws.set_document("g", document(graph(&["models/aml"], request, expressions)));
    ws
}

#[test]
fn nodes_read_the_entity_flat_and_its_references_as_records() {
    let ws = workspace(
        json!({ "target": "transaction" }),
        &[
            ("big", "amount > 9000"),
            ("cash", "channel == 'cash'"),
            ("risky", "customer.risk_rating == 'high'"),
            ("customer_id", "customer.id"),
        ],
    );
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn a_field_the_entity_does_not_have_is_flagged() {
    let ws = workspace(
        json!({ "target": "transaction" }),
        &[("big", "amout > 9000")],
    );
    let errors = codes(&ws, "g", Severity::Error);
    assert!(
        errors.contains(&DiagnosticCode::UndefinedVariable),
        "{errors:?}"
    );
}

#[test]
fn a_value_outside_the_dictionary_is_flagged() {
    let ws = workspace(
        json!({ "target": "transaction" }),
        &[("gold", "channel == 'gold'")],
    );
    let diagnostics = ws.diagnostics("g");
    assert!(!diagnostics.is_empty(), "expected a diagnostic for 'gold'");
}

#[test]
fn an_entity_no_import_makes_visible_is_an_error() {
    let mut ws = Workspace::new();
    ws.set_document("models/aml", document(models(transaction_fields())));
    ws.set_document(
        "g",
        document(graph(
            &[],
            json!({ "target": "transaction" }),
            &[("x", "1")],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let missing = diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::UnknownDataModelTarget)
        .unwrap_or_else(|| panic!("{diagnostics:?}"));
    assert_eq!(missing.severity, Severity::Error);
    assert!(
        missing.message.contains("no entity `transaction` visible"),
        "{}",
        missing.message
    );
}

#[test]
fn a_schema_and_an_entity_together_is_an_error() {
    let schema =
        json!({ "type": "object", "properties": { "amount": { "type": "number" } } }).to_string();
    let ws = workspace(
        json!({ "schema": schema, "target": "transaction" }),
        &[("x", "1")],
    );
    let errors = codes(&ws, "g", Severity::Error);
    assert!(
        errors.contains(&DiagnosticCode::InvalidGraphStructure),
        "{errors:?}"
    );
}

#[test]
fn an_entity_is_a_declared_request() {
    let ws = workspace(json!({ "target": "transaction" }), &[("x", "amount")]);
    let warnings = codes(&ws, "g", Severity::Warning);
    assert!(
        !warnings.contains(&DiagnosticCode::MissingInputSchema),
        "{warnings:?}"
    );
}

#[test]
fn an_empty_target_is_no_target() {
    let ws = workspace(json!({ "target": "" }), &[("x", "1")]);
    let warnings = codes(&ws, "g", Severity::Warning);
    assert!(
        warnings.contains(&DiagnosticCode::MissingInputSchema),
        "{warnings:?}"
    );
}

#[test]
fn a_change_to_the_entity_in_its_policy_reaches_the_graph() {
    let mut ws = workspace(
        json!({ "target": "transaction" }),
        &[("big", "amout > 9000")],
    );
    assert!(codes(&ws, "g", Severity::Error).contains(&DiagnosticCode::UndefinedVariable));
    let mut fields = transaction_fields();
    fields.as_array_mut().unwrap().push(prop("amout", "number"));
    ws.set_document("models/aml", document(models(fields)));
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

async fn run(imports: &[&str], input: Value) -> Result<Value, String> {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::DecisionEngine;

    let loader = std::sync::Arc::new(MemoryLoader::default());
    loader.add("models/aml", document(models(transaction_fields())));
    loader.add(
        "g",
        document(graph(
            imports,
            json!({ "target": "transaction" }),
            &[
                ("big", "amount > 9000"),
                ("risky", "customer.risk_rating == 'high'"),
                ("customer_id", "customer.id"),
            ],
        )),
    );
    let engine = DecisionEngine::default().with_loader(loader);
    engine
        .evaluate("g", input.into())
        .await
        .map(|response| response.result.to_value())
        .map_err(|error| format!("{error:?}"))
}

#[tokio::test]
async fn runtime_reads_the_entity_flat_with_its_reference_as_a_record() {
    let output = run(
        &["models/aml"],
        json!({ "txn_id": "X1", "amount": 9500, "channel": "cash", "customer": { "id": "C1", "risk_rating": "high" } }),
    )
    .await
    .expect("evaluates");
    assert_eq!(output["big"], json!(true), "{output}");
    assert_eq!(output["risky"], json!(true), "{output}");
    assert_eq!(output["customer_id"], json!("C1"), "{output}");
}

#[tokio::test]
async fn runtime_rejects_a_request_that_is_not_the_entity() {
    let error = run(
        &["models/aml"],
        json!({ "txn_id": "X1", "amount": "a lot", "channel": "cash", "customer": { "id": "C1", "risk_rating": "high" } }),
    )
    .await
    .expect_err("must fail");
    assert!(error.contains("not a valid `transaction`"), "{error}");
    assert!(error.contains("'amount'"), "{error}");
}

#[tokio::test]
async fn runtime_rejects_a_reference_sent_as_an_id_alone() {
    let error = run(
        &["models/aml"],
        json!({ "txn_id": "X1", "amount": 1, "channel": "cash", "customer": "C1" }),
    )
    .await
    .expect_err("must fail");
    assert!(
        error.contains("'customer' is the id \\\"C1\\\", but a reference carries its record: send the `customer` itself"),
        "{error}"
    );
}

#[tokio::test]
async fn runtime_errors_when_no_import_makes_the_entity_visible() {
    let error = run(&[], json!({ "txn_id": "X1" }))
        .await
        .expect_err("must fail");
    assert!(error.contains("no entity `transaction` visible"), "{error}");
}

#[test]
fn a_document_says_what_it_reads_from_its_request() {
    let set = |reads: Option<std::collections::BTreeSet<String>>| {
        reads.map(|r| r.into_iter().collect::<Vec<_>>())
    };

    // A graph typed by an entity: its fields under the entity's name, through references.
    let ws = workspace(
        json!({ "target": "transaction" }),
        &[("big", "amount > 9000"), ("risky", "customer.risk_rating == 'high'")],
    );
    assert_eq!(
        set(ws.reads("g")),
        Some(vec!["transaction.amount".to_string(), "transaction.customer.risk_rating".to_string()])
    );

    // A read of the whole request can't be followed.
    let ws = workspace(json!({ "target": "transaction" }), &[("all", "$")]);
    assert_eq!(ws.reads("g"), None);

    // Nor can a function node's.
    let mut ws = Workspace::new();
    ws.set_document("models/aml", document(models(transaction_fields())));
    ws.set_document(
        "f",
        document(json!({
            "imports": ["models/aml"],
            "nodes": [
                node("in", "inputNode", json!({ "target": "transaction" })),
                node("fn", "functionNode", json!({ "source": "export const handler = (input) => input;" })),
                node("out", "outputNode", json!({}))
            ],
            "edges": [edge("e1", "in", "fn"), edge("e2", "fn", "out")]
        })),
    );
    assert_eq!(ws.reads("f"), None);

    // A policy: what its rules read, including those of what it imports.
    let mut ws = Workspace::new();
    ws.set_document("models/aml", document(models(transaction_fields())));
    ws.set_document(
        "p",
        document(json!({
            "imports": ["models/aml"],
            "blocks": [{ "id": "r", "type": "expression", "props": { "data": {
                "key": "transaction.big", "value": "transaction.amount > 9000 and transaction.customer.risk_rating == 'high'"
            } } }]
        })),
    );
    let reads = set(ws.reads("p")).expect("known");
    assert!(reads.contains(&"transaction.amount".to_string()), "{reads:?}");
    assert!(reads.iter().any(|r| r.starts_with("transaction.customer")), "{reads:?}");
}

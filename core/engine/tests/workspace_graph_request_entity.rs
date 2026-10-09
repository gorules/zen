//! A graph's Request node typed by an entity (`inputNode.content.target`)
//! instead of a schema: the entity is visible through the graph's imports,
//! and the request is that entity, flat; a reference holds its record.

use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{DiagnosticCode, ScopeRequest, Severity, Workspace};
use zen_expression::variable::VariableType;

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

#[test]
fn a_read_it_cant_follow_reads_everything() {
    // `$root.x` and another node's output (`$nodes.x`): the request, but not
    // as a field of the entity it's typed by. Unknown, so everything.
    for expression in ["$root.amount > 9000", "$nodes.in.amount > 9000"] {
        let ws = workspace(json!({ "target": "transaction" }), &[("big", expression)]);
        assert_eq!(ws.reads("g"), None, "{expression}");
    }
}

#[test]
fn reads_through_an_index_or_alias_are_never_lost() {
    // Each is either followed (the entity's field is listed) or unknown
    // (None: everything is computed); never silently left out.
    for expression in [
        "customer['risk_rating'] == 'high'",
        "some([customer] as c, c.risk_rating == 'high')",
        "count([amount] as a, a > 9000) > 0",
    ] {
        let ws = workspace(json!({ "target": "transaction" }), &[("x", expression)]);
        if let Some(reads) = ws.reads("g") {
            assert!(!reads.is_empty(), "{expression}: reads nothing");
        }
    }
}

fn input<'a>(
    inputs: &'a [zen_engine::policy::InputProperty],
    path: &str,
) -> &'a zen_engine::policy::InputProperty {
    inputs
        .iter()
        .find(|i| i.path.as_ref() == path)
        .unwrap_or_else(|| panic!("{path} in {inputs:?}"))
}

fn object_fields(ty: &VariableType) -> Vec<String> {
    let VariableType::Object(fields) = ty else {
        panic!("expected an object, got {ty:?}");
    };
    let mut keys: Vec<String> = fields.borrow().keys().map(|k| k.to_string()).collect();
    keys.sort();
    keys
}

#[test]
fn inputs_carry_a_reference_as_its_record() {
    // As nodes read it and the runtime takes it: `customer` is the record,
    // and so is a reference inside a relationship's records.
    let mut fields = transaction_fields();
    fields.as_array_mut().unwrap().push(
        json!({ "id": "lines", "name": "lines", "type": "relationship", "target": "line", "array": true, "optional": false }),
    );
    let mut models = models(fields);
    models["blocks"].as_array_mut().unwrap().push(entity(
        "b-line",
        "line",
        json!([
            prop("qty", "number"),
            { "id": "product", "name": "product", "type": "reference", "target": "customer", "array": false, "optional": false }
        ]),
    ));
    let mut ws = Workspace::new();
    ws.set_document("models/aml", document(models));
    ws.set_document(
        "g",
        document(graph(&["models/aml"], json!({ "target": "transaction" }), &[("x", "amount")])),
    );
    let inputs = ws.inputs(&ScopeRequest::for_policy("g"));
    let customer = input(&inputs, "customer");
    assert_eq!(object_fields(&customer.resolved_type), vec!["id", "risk_rating"]);
    assert!(!customer.optional);
    let VariableType::Array(line) = &input(&inputs, "lines").resolved_type else {
        panic!("lines: {inputs:?}");
    };
    let VariableType::Object(line) = line.as_ref() else {
        panic!("line: {line:?}");
    };
    let product = line.borrow().get("product").cloned().expect("product");
    assert_eq!(object_fields(&product), vec!["id", "risk_rating"]);
    // Which inputs are references, and to what: a host that fills records
    // in from ids takes the id there.
    assert_eq!(customer.reference.as_deref(), Some("customer"));
    let lines = input(&inputs, "lines");
    assert_eq!(lines.reference, None);
    assert_eq!(
        lines.record_references.get("product").map(|t| t.as_ref()),
        Some("customer")
    );
    assert_eq!(input(&inputs, "amount").reference, None);
    assert!(input(&inputs, "amount").record_references.is_empty());
}

#[test]
fn an_optional_field_is_nullable_in_a_graph() {
    let mut fields = transaction_fields();
    fields.as_array_mut().unwrap().push(
        json!({ "id": "fee", "name": "fee", "type": "number", "array": false, "optional": true }),
    );
    let mut ws = Workspace::new();
    ws.set_document("models/aml", document(models(fields)));
    for (expression, flagged) in [("fee * 2", true), ("(fee ?? 0) * 2", false)] {
        ws.set_document(
            "g",
            document(graph(&["models/aml"], json!({ "target": "transaction" }), &[("x", expression)])),
        );
        let errors = codes(&ws, "g", Severity::Error);
        assert_eq!(!errors.is_empty(), flagged, "{expression}: {errors:?}");
    }
    let inputs = ws.inputs(&ScopeRequest::for_policy("g"));
    let fee = input(&inputs, "fee");
    assert!(fee.optional);
    assert!(matches!(fee.resolved_type, VariableType::Nullable(_)), "{:?}", fee.resolved_type);
}

/// `g` imports `a` and `b`, both import `d` (a diamond). `b` declares
/// `transaction.amount` a string, `d` a number with `txn_id`; `e`, imported
/// by `b`, adds `flag` to it. Walked breadth-first (a, b, d, e), `b`'s
/// `amount` comes first; every declaration's fields are merged.
fn diamond() -> Vec<(&'static str, Value)> {
    let policy = |imports: Value, blocks: Value| json!({ "imports": imports, "blocks": blocks });
    vec![
        ("a", policy(json!(["d"]), json!([]))),
        ("b", policy(json!(["d", "e"]), json!([entity("b-t", "transaction", json!([prop("amount", "string")]))]))),
        (
            "d",
            policy(json!([]), json!([entity("d-t", "transaction", json!([prop("amount", "number"), prop("txn_id", "string")]))])),
        ),
        ("e", policy(json!([]), json!([entity("e-t", "transaction", json!([prop("flag", "boolean")]))]))),
    ]
}

fn diamond_graph() -> Value {
    graph(
        &["a", "b"],
        json!({ "target": "transaction" }),
        &[("shout", "amount + '!'"), ("id", "txn_id"), ("flagged", "flag == true")],
    )
}

#[test]
fn imports_are_walked_and_entities_merged_as_at_runtime() {
    let mut ws = Workspace::new();
    for (path, policy) in diamond() {
        ws.set_document(path, document(policy));
    }
    ws.set_document("g", document(diamond_graph()));
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let inputs = ws.inputs(&ScopeRequest::for_policy("g"));
    let paths: Vec<&str> = inputs.iter().map(|i| i.path.as_ref()).collect();
    assert_eq!(paths, vec!["amount", "flag", "txn_id"]);
    assert!(matches!(input(&inputs, "amount").resolved_type, VariableType::String));
}

#[tokio::test]
async fn runtime_walks_imports_and_merges_entities_as_the_editor() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::DecisionEngine;

    let loader = std::sync::Arc::new(MemoryLoader::default());
    for (path, policy) in diamond() {
        loader.add(path, document(policy));
    }
    loader.add("g", document(diamond_graph()));
    let engine = DecisionEngine::default().with_loader(loader);
    let output = engine
        .evaluate("g", json!({ "amount": "high", "txn_id": "X1", "flag": true }).into())
        .await
        .map(|response| response.result.to_value())
        .map_err(|error| format!("{error:?}"))
        .expect("a string amount is the entity's");
    assert_eq!(output["shout"], json!("high!"), "{output}");
    assert_eq!(output["flagged"], json!(true), "{output}");
    let error = engine
        .evaluate("g", json!({ "amount": 1, "txn_id": "X1", "flag": "yes" }).into())
        .await
        .map_err(|error| format!("{error:?}"))
        .expect_err("must fail");
    assert!(error.contains("'amount'") && error.contains("'flag'"), "{error}");
}

#[test]
fn entities_referencing_each_other_type_quickly() {
    // Eight entities, each referencing every other: opened a few links deep,
    // not along every path through them.
    let names: Vec<String> = (0..8).map(|i| format!("e{i}")).collect();
    let blocks: Vec<Value> = names
        .iter()
        .map(|name| {
            let mut fields = vec![prop("id", "string")];
            for other in names.iter().filter(|o| *o != name) {
                fields.push(json!({ "id": other, "name": other, "type": "reference", "target": other, "array": false, "optional": false }));
            }
            entity(&format!("b-{name}"), name, json!(fields))
        })
        .collect();
    let started = std::time::Instant::now();
    let mut ws = Workspace::new();
    ws.set_document("models/ring", document(json!({ "imports": [], "blocks": blocks })));
    ws.set_document(
        "g",
        document(graph(
            &["models/ring"],
            json!({ "target": "e0" }),
            &[("deep", "e1.e2.e3.e4.id"), ("far", "e1.e2.e3.e4.e5.id")],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let inputs = ws.inputs(&ScopeRequest::for_policy("g"));
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "{:?}", started.elapsed());
    // Four links are records; a fifth is unknown.
    let errors: Vec<_> = diagnostics.iter().filter(|d| d.severity == Severity::Error).collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].code == DiagnosticCode::ImplicitAny && errors[0].message.contains("`far`"), "{errors:?}");
    assert_eq!(inputs.len(), 8);
}

#[test]
fn a_graph_without_an_entity_request_ignores_entity_edits() {
    let schema =
        json!({ "type": "object", "properties": { "amount": { "type": "number" } } }).to_string();
    for (request, affected) in [(json!({ "schema": schema }), false), (json!({ "target": "transaction" }), true)] {
        let mut ws = Workspace::new();
        ws.set_document("models/aml", document(models(transaction_fields())));
        ws.set_document("g", document(graph(&["models/aml"], request.clone(), &[("x", "amount")])));
        let _ = ws.diagnostics("g");
        let (cursor, _) = ws.changes_since(0);
        let mut fields = transaction_fields();
        fields.as_array_mut().unwrap().push(prop("extra", "number"));
        ws.set_document("models/aml", document(models(fields)));
        let (_, changed) = ws.changes_since(cursor);
        assert_eq!(
            changed.iter().any(|p| p.as_ref() == "g"),
            affected,
            "{request}: {changed:?}"
        );
    }
}

#[tokio::test]
async fn a_decision_loads_its_request_entity_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use zen_engine::loader::ClosureLoader;
    use zen_engine::model::GraphContent;
    use zen_engine::Decision;

    let loads = Arc::new(AtomicUsize::new(0));
    let counted = loads.clone();
    let models = Arc::new(document(models(transaction_fields())));
    let loader = ClosureLoader::new(move |_key: String| {
        counted.fetch_add(1, Ordering::SeqCst);
        let models = models.clone();
        async move { Ok(models) }
    });
    let content: GraphContent = serde_json::from_value(graph(
        &["models/aml"],
        json!({ "target": "transaction" }),
        &[("big", "amount > 9000")],
    ))
    .unwrap();
    let decision = Decision::from(content).with_loader(Arc::new(loader));
    let request = json!({ "txn_id": "X1", "amount": 9500, "channel": "cash", "customer": { "id": "C1", "risk_rating": "low" } });
    for _ in 0..3 {
        let output = decision.evaluate(request.clone().into()).await.expect("evaluates");
        assert_eq!(output.result.to_value()["big"], json!(true));
    }
    assert_eq!(loads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runtime_rejects_a_schema_and_an_entity_together() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::DecisionEngine;

    let schema =
        json!({ "type": "object", "properties": { "amount": { "type": "number" } } }).to_string();
    let loader = std::sync::Arc::new(MemoryLoader::default());
    loader.add("models/aml", document(models(transaction_fields())));
    loader.add(
        "g",
        document(graph(
            &["models/aml"],
            json!({ "schema": schema, "target": "transaction" }),
            &[("x", "1")],
        )),
    );
    let engine = DecisionEngine::default().with_loader(loader);
    let error = engine
        .evaluate("g", json!({ "amount": 1 }).into())
        .await
        .map_err(|error| format!("{error:?}"))
        .expect_err("must fail");
    assert!(
        error.contains("the request is typed twice: by a schema and by an entity (`target`); keep one"),
        "{error}"
    );
}

#[test]
fn reads_of_an_unknown_document_are_unknown() {
    let ws = workspace(json!({ "target": "transaction" }), &[("x", "amount")]);
    assert_eq!(ws.reads("missing"), None);
}

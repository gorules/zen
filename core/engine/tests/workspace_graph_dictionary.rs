use std::sync::Arc;

use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{Cursor, CursorTarget, DiagnosticCode, ScopeRequest, Severity, Workspace};
use zen_expression::nl::{EditHint, NlTokenKind};

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn edge(id: &str, source: &str, target: &str) -> Value {
    json!({ "id": id, "sourceId": source, "targetId": target, "sourceHandle": null })
}

fn dictionary_policy(imports: &[&str], name: &str, entries: &[(&str, &str)]) -> Value {
    json!({
        "imports": imports,
        "blocks": [{
            "id": "dict1",
            "type": "dictionary",
            "props": {
                "data": {
                    "name": name,
                    "entries": entries.iter().enumerate().map(|(i, (value, label))| json!({
                        "id": format!("e{i}"),
                        "value": value,
                        "label": label,
                    })).collect::<Vec<_>>(),
                }
            }
        }]
    })
}

fn tier_table(cell: &str) -> Value {
    node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [
                { "id": "c1", "name": "Age", "field": "age" }
            ],
            "outputs": [
                { "id": "o1", "name": "Tier", "field": "tier", "type": "customerTier" }
            ],
            "rules": [
                { "_id": "r1", "c1": "> 18", "o1": cell },
                { "_id": "r2", "c1": "", "o1": "'STD'" }
            ]
        }),
    )
}

fn graph(imports: &[&str], middle: Value) -> Value {
    let schema = json!({
        "type": "object",
        "properties": { "age": { "type": "number" } },
        "required": ["age"]
    });
    let middle_id = middle["id"].as_str().unwrap().to_string();
    json!({
        "imports": imports,
        "nodes": [
            node("in", "inputNode", json!({ "schema": schema.to_string() })),
            middle,
            node("out", "outputNode", json!({}))
        ],
        "edges": [
            edge("e1", "in", &middle_id),
            edge("e2", &middle_id, "out")
        ]
    })
}

fn error_codes(ws: &Workspace, path: &str) -> Vec<DiagnosticCode> {
    ws.diagnostics(path)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn imported_dictionary_types_output_column() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document("g", document(graph(&["dicts"], tier_table("'VIP'"))));
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn imported_dictionary_rejects_non_member_value() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document("g", document(graph(&["dicts"], tier_table("'GOLD'"))));
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::TypeMismatch),
        "expected type mismatch, got {codes:?}"
    );
}

#[test]
fn dictionary_type_without_import_is_unknown() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important")],
        )),
    );
    ws.set_document("g", document(graph(&[], tier_table("'VIP'"))));
    let unknown = ws.diagnostics("g").into_iter().any(|d| {
        d.code == DiagnosticCode::TypeMismatch && d.message.contains("unknown output type")
    });
    assert!(unknown, "{:?}", ws.diagnostics("g"));
}

#[test]
fn transitively_imported_dictionary_is_in_scope() {
    let mut ws = Workspace::new();
    ws.set_document(
        "base",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document(
        "mid",
        document(json!({ "imports": ["base"], "blocks": [] })),
    );
    ws.set_document("g", document(graph(&["mid"], tier_table("'VIP'"))));
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn reverse_imported_dictionary_is_not_in_scope() {
    let mut ws = Workspace::new();
    ws.set_document("base", document(json!({ "imports": [], "blocks": [] })));
    ws.set_document(
        "catalog",
        document(dictionary_policy(
            &["base"],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document("g", document(graph(&["base"], tier_table("'VIP'"))));
    let unknown = ws.diagnostics("g").into_iter().any(|d| {
        d.code == DiagnosticCode::TypeMismatch && d.message.contains("unknown output type")
    });
    assert!(unknown, "{:?}", ws.diagnostics("g"));
}

#[test]
fn missing_or_graph_imports_are_diagnosed() {
    let mut ws = Workspace::new();
    ws.set_document("other", document(graph(&[], tier_table("'VIP'"))));
    ws.set_document(
        "g",
        document(graph(&["missing", "other"], tier_table("'VIP'"))),
    );
    let not_found: Vec<String> = ws
        .diagnostics("g")
        .into_iter()
        .filter(|d| d.code == DiagnosticCode::ImportNotFound)
        .map(|d| d.message.clone())
        .collect();
    assert_eq!(not_found.len(), 2, "{not_found:?}");
    assert!(not_found.iter().any(|m| m.contains("'missing'")));
    assert!(not_found
        .iter()
        .any(|m| m.contains("'other'") && m.contains("graph")));
}

#[test]
fn dictionaries_query_covers_graph_imports() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document("g", document(graph(&["dicts"], tier_table("'VIP'"))));
    let dictionaries = ws.dictionaries(&ScopeRequest {
        policy_path: Arc::from("g"),
        goals: Vec::new(),
    });
    assert_eq!(dictionaries.len(), 1, "{dictionaries:?}");
    assert_eq!(dictionaries[0].name.as_ref(), "customerTier");
    assert_eq!(dictionaries[0].source.as_ref(), "dicts");
    assert_eq!(dictionaries[0].entries[0].label.as_ref(), "Very important");
}

#[test]
fn dictionary_edit_invalidates_graph_analysis() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document("g", document(graph(&["dicts"], tier_table("'VIP'"))));
    assert!(ws.diagnostics("g").is_empty());

    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("GOLD", "Gold"), ("STD", "Standard")],
        )),
    );
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::TypeMismatch),
        "expected type mismatch after dictionary edit, got {codes:?}"
    );
}

#[test]
fn nl_tokenize_graph_cell_uses_dictionary_labels() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document("g", document(graph(&["dicts"], tier_table("'VIP'"))));

    let cursor = Cursor {
        policy_path: "g".into(),
        block_id: "dt".into(),
        pos: 0,
        target: CursorTarget::DecisionTableCell {
            row: "r1".into(),
            col: "o1".into(),
        },
    };
    let result = ws.nl_tokenize(&cursor, "'VIP'").expect("cursor resolves");
    let str_tok = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .expect("string token present");
    let Some(EditHint::Select { options }) = str_tok.hint else {
        panic!("expected select hint, got {:?}", str_tok.hint);
    };
    let options = &result.enums[options as usize];
    let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["Very important", "Standard"]);
}

fn schema_graph(imports: &[&str], tier_schema: Value, output_schema: Option<Value>) -> Value {
    let schema = json!({
        "type": "object",
        "properties": { "tier": tier_schema },
        "required": ["tier"]
    });
    let mut nodes = vec![
        node("in", "inputNode", json!({ "schema": schema.to_string() })),
        node(
            "calc",
            "expressionNode",
            json!({ "passThrough": true, "expressions": [{ "id": "e1", "key": "vip", "value": "tier == 'VIP'" }] }),
        ),
    ];
    nodes.push(match output_schema {
        Some(out) => node("out", "outputNode", json!({ "schema": out.to_string() })),
        None => node("out", "outputNode", json!({})),
    });
    json!({
        "imports": imports,
        "nodes": nodes,
        "edges": [edge("e1", "in", "calc"), edge("e2", "calc", "out")]
    })
}

#[test]
fn input_schema_dictionary_types_field_as_enum() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    ws.set_document(
        "g",
        document(schema_graph(
            &["dicts"],
            json!({ "$dictionary": "customerTier" }),
            None,
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let inputs = ws.inputs(&ScopeRequest {
        policy_path: Arc::from("g"),
        goals: Vec::new(),
    });
    let tier = inputs
        .iter()
        .find(|p| p.path.as_ref() == "tier")
        .expect("tier input");
    assert_eq!(format!("{}", tier.resolved_type), "customerTier");
}

#[test]
fn input_schema_dictionary_membership_checks_expressions() {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    let mut graph = schema_graph(&["dicts"], json!({ "$dictionary": "customerTier" }), None);
    graph["nodes"][1]["content"]["expressions"][0]["value"] = json!("tier == 'GOLD'");
    ws.set_document("g", document(graph));
    let membership = ws.diagnostics("g").into_iter().any(|d| {
        d.severity == Severity::Error && d.message.contains("not a valid member of `customerTier`")
    });
    assert!(membership, "{:?}", ws.diagnostics("g"));
}

#[test]
fn input_schema_unknown_dictionary_is_diagnosed() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(schema_graph(
            &[],
            json!({ "$dictionary": "customerTier" }),
            None,
        )),
    );
    let unknown = ws.diagnostics("g").into_iter().any(|d| {
        d.code == DiagnosticCode::TypeMismatch
            && d.message.contains("unknown dictionary 'customerTier'")
    });
    assert!(unknown, "{:?}", ws.diagnostics("g"));
}

#[tokio::test]
async fn runtime_validates_dictionary_membership() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::DecisionEngine;

    let loader = std::sync::Arc::new(MemoryLoader::default());
    loader.add(
        "dicts",
        serde_json::from_value::<DecisionContent>(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        ))
        .unwrap(),
    );
    loader.add(
        "g",
        serde_json::from_value::<DecisionContent>(schema_graph(
            &["dicts"],
            json!({ "$dictionary": "customerTier" }),
            Some(json!({
                "type": "object",
                "properties": { "tier": { "$dictionary": "customerTier" } }
            })),
        ))
        .unwrap(),
    );
    let engine = DecisionEngine::default().with_loader(loader);

    let ok = engine.evaluate("g", json!({ "tier": "VIP" }).into()).await;
    assert!(ok.is_ok(), "{ok:?}");

    let bad = engine.evaluate("g", json!({ "tier": "GOLD" }).into()).await;
    let message = format!("{:?}", bad.expect_err("must fail validation"));
    assert!(
        message.contains("Validation") || message.to_lowercase().contains("enum"),
        "{message}"
    );
}

#[tokio::test]
async fn runtime_errors_on_unknown_schema_dictionary() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::DecisionEngine;

    let loader = std::sync::Arc::new(MemoryLoader::default());
    loader.add(
        "g",
        serde_json::from_value::<DecisionContent>(schema_graph(
            &[],
            json!({ "$dictionary": "customerTier" }),
            None,
        ))
        .unwrap(),
    );
    let engine = DecisionEngine::default().with_loader(loader);
    let result = engine.evaluate("g", json!({ "tier": "VIP" }).into()).await;
    let message = format!("{:?}", result.expect_err("must fail"));
    assert!(
        message.contains("unknown dictionary 'customerTier'"),
        "{message}"
    );
}

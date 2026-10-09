use std::sync::Arc;

use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::Workspace;

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn graph(imports: &[&str], decision_keys: &[&str]) -> Value {
    let mut nodes = vec![node("in", "inputNode", json!({}))];
    for (i, key) in decision_keys.iter().enumerate() {
        nodes.push(node(
            &format!("call{i}"),
            "decisionNode",
            json!({ "key": key }),
        ));
    }
    json!({ "imports": imports, "nodes": nodes, "edges": [] })
}

fn policy(imports: &[&str]) -> Value {
    json!({ "imports": imports, "blocks": [] })
}

fn moves(pairs: &[(&str, &str)]) -> Vec<(Arc<str>, Arc<str>)> {
    pairs
        .iter()
        .map(|(from, to)| (Arc::from(*from), Arc::from(*to)))
        .collect()
}

fn edits(ws: &Workspace, pairs: &[(&str, &str)]) -> Vec<Value> {
    ws.move_paths(&moves(pairs))
        .into_iter()
        .map(|edit| serde_json::to_value(edit).unwrap())
        .collect()
}

#[test]
fn graph_imports_follow_a_moved_policy() {
    let mut ws = Workspace::new();
    ws.set_document("loan-dictionaries", document(policy(&[])));
    ws.set_document("loan", document(graph(&["loan-dictionaries"], &[])));

    assert_eq!(
        edits(&ws, &[("loan-dictionaries", "shared/loan-dictionaries")]),
        vec![json!({
            "kind": "replaceImport",
            "document": "loan",
            "from": "loan-dictionaries",
            "to": "shared/loan-dictionaries",
        })]
    );
}

#[test]
fn policy_imports_follow_a_moved_policy() {
    let mut ws = Workspace::new();
    ws.set_document("base", document(policy(&[])));
    ws.set_document("pricing", document(policy(&["base"])));

    assert_eq!(
        edits(&ws, &[("base", "core/base")]),
        vec![json!({
            "kind": "replaceImport",
            "document": "pricing",
            "from": "base",
            "to": "core/base",
        })]
    );
}

#[test]
fn decision_nodes_follow_a_moved_graph() {
    let mut ws = Workspace::new();
    ws.set_document("risk", document(graph(&[], &[])));
    ws.set_document("main", document(graph(&[], &["risk", "other", "risk"])));

    assert_eq!(
        edits(&ws, &[("risk", "checks/risk")]),
        vec![
            json!({
                "kind": "replaceDecisionKey",
                "document": "main",
                "nodeId": "call0",
                "from": "risk",
                "to": "checks/risk",
            }),
            json!({
                "kind": "replaceDecisionKey",
                "document": "main",
                "nodeId": "call2",
                "from": "risk",
                "to": "checks/risk",
            }),
        ]
    );
}

#[test]
fn folder_move_rewrites_references_between_moved_documents() {
    let mut ws = Workspace::new();
    ws.set_document("loan/dictionaries", document(policy(&[])));
    ws.set_document(
        "loan/approval",
        document(graph(&["loan/dictionaries"], &[])),
    );
    ws.set_document("main", document(graph(&[], &["loan/approval"])));

    assert_eq!(
        edits(
            &ws,
            &[
                ("loan/approval", "lending/approval"),
                ("loan/dictionaries", "lending/dictionaries"),
            ]
        ),
        vec![
            json!({
                "kind": "replaceDecisionKey",
                "document": "main",
                "nodeId": "call0",
                "from": "loan/approval",
                "to": "lending/approval",
            }),
            json!({
                "kind": "replaceImport",
                "document": "loan/approval",
                "from": "loan/dictionaries",
                "to": "lending/dictionaries",
            }),
        ]
    );
}

#[test]
fn references_to_missing_documents_are_still_rewritten() {
    let mut ws = Workspace::new();
    ws.set_document("loan", document(graph(&["loan-dictionaries"], &[])));

    assert_eq!(edits(&ws, &[("loan-dictionaries", "renamed")]).len(), 1);
}

#[test]
fn unrelated_and_identity_moves_produce_no_edits() {
    let mut ws = Workspace::new();
    ws.set_document("a", document(policy(&[])));
    ws.set_document("b", document(graph(&["a"], &["a"])));

    assert!(edits(&ws, &[("a", "a"), ("c", "d")]).is_empty());
}

#[test]
fn graph_importing_and_calling_the_same_path_gets_both_edits() {
    let mut ws = Workspace::new();
    ws.set_document("a", document(policy(&[])));
    ws.set_document("b", document(graph(&["a"], &["a"])));

    let kinds: Vec<String> = edits(&ws, &[("a", "z")])
        .into_iter()
        .map(|edit| edit["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, vec!["replaceImport", "replaceDecisionKey"]);
}

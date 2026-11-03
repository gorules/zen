use std::sync::Arc;

use serde_json::{json, Value};
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationError};
use zen_expression::variable::Variable;

fn policy_content() -> DecisionContent {
    policy_content_with_threshold(100)
}

fn policy_content_with_threshold(threshold: i64) -> DecisionContent {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "amount", "type": "number", "array": false, "optional": false }
                ]
            }}},
            { "id": "assert", "type": "assertion", "props": { "data": {
                "output": "approved",
                "conditions": [
                    { "id": "c1", "expression": format!("amount >= {threshold}"), "operator": "and", "depth": 0 }
                ]
            }}}
        ]
    });
    serde_json::from_value(doc).unwrap()
}

fn engine_with_policy(precompile: bool) -> DecisionEngine {
    let loader = MemoryLoader::default();
    loader.add("policy", policy_content());
    let engine = DecisionEngine::default().with_loader(Arc::new(loader));
    if precompile {
        engine.compile();
    }
    engine
}

fn input(amount: i64) -> Variable {
    Variable::from(json!({ "amount": amount }))
}

fn approved(result: &Variable) -> Value {
    serde_json::to_value(result).unwrap()["approved"].clone()
}

#[tokio::test]
async fn precompiled_matches_lazy() {
    let lazy = engine_with_policy(false)
        .evaluate("policy", input(150))
        .await
        .unwrap();
    let eager = engine_with_policy(true)
        .evaluate("policy", input(150))
        .await
        .unwrap();

    assert_eq!(
        serde_json::to_value(&lazy.result).unwrap(),
        serde_json::to_value(&eager.result).unwrap()
    );
    assert_eq!(approved(&eager.result), json!(true));
}

#[tokio::test]
async fn compiled_resolves_cross_policy_imports() {
    let loader = MemoryLoader::default();
    loader.add(
        "base.json",
        serde_json::from_value::<DecisionContent>(json!({
            "imports": [],
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "platform", "scope": "global",
                    "properties": [
                        { "id": "g1", "name": "amount", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "qual", "type": "assertion", "props": { "data": {
                    "output": "qualified",
                    "conditions": [
                        { "id": "c1", "expression": "amount >= 100", "operator": "and", "depth": 0 }
                    ]
                }}}
            ]
        }))
        .unwrap(),
    );
    loader.add(
        "main.json",
        serde_json::from_value::<DecisionContent>(json!({
            "imports": ["base.json"],
            "blocks": [
                { "id": "appr", "type": "assertion", "props": { "data": {
                    "output": "approved",
                    "conditions": [
                        { "id": "c1", "expression": "qualified", "operator": "and", "depth": 0 }
                    ]
                }}}
            ]
        }))
        .unwrap(),
    );

    let engine = DecisionEngine::default().with_loader(Arc::new(loader));
    engine.compile();

    let result = engine.evaluate("main.json", input(150)).await.unwrap();
    let json = serde_json::to_value(&result.result).unwrap();
    assert_eq!(json["qualified"], serde_json::json!(true));
    assert_eq!(json["approved"], serde_json::json!(true));
}

fn parse_error_policy() -> DecisionContent {
    serde_json::from_value(json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "platform", "scope": "global",
                "properties": [
                    { "id": "g1", "name": "amount", "type": "number", "array": false, "optional": false }
                ]
            }}},
            { "id": "a", "type": "assertion", "props": { "data": {
                "output": "x",
                "conditions": [
                    { "id": "c1", "expression": "if (( invalid )) {{", "operator": "and", "depth": 0 }
                ]
            }}}
        ]
    }))
    .unwrap()
}

fn invalid_graph() -> DecisionContent {
    serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "type": "inputNode", "name": "Input", "position": { "x": 0, "y": 0 } }
        ],
        "edges": [
            { "id": "e1", "type": "edge", "sourceId": "in", "targetId": "ghost" }
        ]
    }))
    .unwrap()
}

#[test]
fn compile_reports_every_failure() {
    let loader = MemoryLoader::default();
    loader.add("good.json", policy_content());
    loader.add("bad-1.json", parse_error_policy());
    loader.add("bad-2.json", parse_error_policy());
    let engine = DecisionEngine::default().with_loader(Arc::new(loader));

    let failures = engine.compile();
    let keys: Vec<&str> = failures.iter().map(|f| f.key.as_ref()).collect();
    assert!(
        keys.contains(&"bad-1.json"),
        "every failure must be listed; got: {keys:?}"
    );
    assert!(
        keys.contains(&"bad-2.json"),
        "every failure must be listed; got: {keys:?}"
    );
}

#[tokio::test]
async fn compile_evicts_bad_keeps_good() {
    let loader = MemoryLoader::default();
    loader.add("good.json", policy_content());
    loader.add("bad-policy.json", parse_error_policy());
    loader.add("bad-graph.json", invalid_graph());
    let engine = DecisionEngine::default().with_loader(Arc::new(loader));

    let failures = engine.compile();
    assert_eq!(failures.len(), 2);
    assert!(failures
        .iter()
        .any(|f| f.kind == "policy" && f.key.as_ref() == "bad-policy.json"));
    assert!(failures
        .iter()
        .any(|f| f.kind == "graph" && f.key.as_ref() == "bad-graph.json"));
    assert_eq!(engine.compile_failures().len(), 2);

    let result = engine.evaluate("good.json", input(150)).await.unwrap();
    assert_eq!(approved(&result.result), json!(true));
}

#[test]
fn compile_surfaces_bundle_errors() {
    let loader = MemoryLoader::default();
    let broken = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "amount", "type": "number", "array": false, "optional": false }
                ]
            }}},
            { "id": "assert", "type": "assertion", "props": { "data": {
                "output": "approved",
                "conditions": [
                    { "id": "c1", "expression": "if (( invalid )) {{", "operator": "and", "depth": 0 }
                ]
            }}}
        ]
    });
    loader.add(
        "policy",
        serde_json::from_value::<DecisionContent>(broken).unwrap(),
    );
    let engine = DecisionEngine::default().with_loader(Arc::new(loader));

    assert!(!engine.compile().is_empty());
}

#[tokio::test]
async fn clone_with_loader_does_not_share_compiled_set() {
    let loader_a = MemoryLoader::default();
    loader_a.add("policy", policy_content_with_threshold(100));
    let engine_a = DecisionEngine::default().with_loader(Arc::new(loader_a));
    engine_a.compile();

    let loader_b = MemoryLoader::default();
    loader_b.add("policy", policy_content_with_threshold(1000));
    let engine_b = engine_a.clone().with_loader(Arc::new(loader_b));
    engine_b.compile();

    let from_a = engine_a.evaluate("policy", input(150)).await.unwrap();
    assert_eq!(approved(&from_a.result), json!(true));

    let from_b = engine_b.evaluate("policy", input(150)).await.unwrap();
    assert_eq!(approved(&from_b.result), json!(false));
}

#[tokio::test]
async fn compile_reports_policies_with_broken_imports() {
    let loader = MemoryLoader::default();
    loader.add("base.json", parse_error_policy());
    loader.add(
        "main.json",
        serde_json::from_value::<DecisionContent>(json!({
            "imports": ["base.json"],
            "blocks": [
                { "id": "appr", "type": "assertion", "props": { "data": {
                    "output": "approved",
                    "conditions": [
                        { "id": "c1", "expression": "amount >= 100", "operator": "and", "depth": 0 }
                    ]
                }}}
            ]
        }))
        .unwrap(),
    );
    let engine = DecisionEngine::default().with_loader(Arc::new(loader));

    let failures = engine.compile();
    assert!(failures
        .iter()
        .any(|f| f.kind == "policy" && f.key.as_ref() == "base.json"));
    let main_failure = failures
        .iter()
        .find(|f| f.key.as_ref() == "main.json")
        .expect("main.json must fail compilation when its import is broken");
    assert_eq!(main_failure.kind, "policy");
    assert!(!main_failure.diagnostics.is_empty());

    let err = engine
        .evaluate("main.json", input(150))
        .await
        .expect_err("main.json must not evaluate through a broken fast path");
    assert!(
        matches!(
            *err,
            EvaluationError::Policy(zen_engine::policy::EvaluationError::CompilationErrors { .. })
        ),
        "expected CompilationErrors, got: {err:?}"
    );
}

#[tokio::test]
async fn reload_recompiles_from_loader() {
    let engine = engine_with_policy(true);

    let denied = engine.evaluate("policy", input(50)).await.unwrap();
    assert_eq!(approved(&denied.result), json!(false));

    engine.compile();

    let allowed = engine.evaluate("policy", input(150)).await.unwrap();
    assert_eq!(approved(&allowed.result), json!(true));
}

use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zen_engine::loader::{LoaderError, MemoryLoader};
use zen_engine::model::{DecisionContent, PolicyContent};
use zen_engine::{
    DecisionEngine, EvaluationError, EvaluationOptions, EvaluationSerializedOptions,
    EvaluationTraceKind,
};

mod support;
use support::load_test_data;

fn simple_policy_json() -> serde_json::Value {
    json!({
        "blocks": [
            {
                "id": "dm-customer",
                "type": "dataModel",
                "props": {
                    "data": {
                        "name": "customer",
                        "properties": [
                            { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                        ]
                    }
                },
                "children": []
            },
            {
                "id": "assert1",
                "type": "assertion",
                "props": {
                    "data": {
                        "output": "customer.isAdult",
                        "conditions": [
                            { "id": "c1", "expression": "customer.age >= 18", "operator": "and", "depth": 0 }
                        ]
                    }
                },
                "children": []
            }
        ]
    })
}

fn importing_policy_json(import_path: &str, output: &str) -> serde_json::Value {
    json!({
        "imports": [import_path],
        "blocks": [
            {
                "id": format!("assert-{output}"),
                "type": "assertion",
                "props": {
                    "data": {
                        "output": format!("customer.{output}"),
                        "conditions": [
                            { "id": "c1", "expression": "customer.age >= 18", "operator": "and", "depth": 0 }
                        ]
                    }
                },
                "children": []
            }
        ]
    })
}

fn make_policy_content(json: serde_json::Value) -> DecisionContent {
    let policy: zen_engine::policy::PolicyDocument =
        serde_json::from_value(json).expect("valid policy fixture");
    DecisionContent::Policy(PolicyContent(Arc::new(policy)))
}

fn engine_with(loader: Arc<MemoryLoader>) -> DecisionEngine {
    DecisionEngine::default().with_loader(loader)
}

#[tokio::test]
async fn evaluate_single_policy() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("policy", make_policy_content(simple_policy_json()));
    let engine = engine_with(loader);

    let result = engine
        .evaluate("policy", json!({ "customer": { "age": 30 } }).into())
        .await
        .expect("evaluate ok");

    let result_json: serde_json::Value = result.result.into();
    assert_eq!(
        result_json.pointer("/customer/isAdult"),
        Some(&json!(true)),
        "expected customer.isAdult=true, got {result_json:#?}",
    );
}

#[tokio::test]
async fn evaluate_with_single_import() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("base", make_policy_content(simple_policy_json()));
    loader.add(
        "entry",
        make_policy_content(importing_policy_json("base", "fromEntry")),
    );
    let engine = engine_with(loader);

    let result = engine
        .evaluate("entry", json!({ "customer": { "age": 70 } }).into())
        .await
        .expect("evaluate ok");

    let result_json: serde_json::Value = result.result.into();
    assert_eq!(result_json.pointer("/customer/isAdult"), Some(&json!(true)));
    assert_eq!(
        result_json.pointer("/customer/fromEntry"),
        Some(&json!(true))
    );
}

#[tokio::test]
async fn evaluate_chained_imports() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("base", make_policy_content(simple_policy_json()));
    loader.add(
        "middle",
        make_policy_content(importing_policy_json("base", "fromMiddle")),
    );
    loader.add(
        "entry",
        make_policy_content(importing_policy_json("middle", "fromEntry")),
    );
    let engine = engine_with(loader);

    let result = engine
        .evaluate("entry", json!({ "customer": { "age": 80 } }).into())
        .await
        .expect("evaluate ok");

    let result_json: serde_json::Value = result.result.into();
    assert_eq!(result_json.pointer("/customer/isAdult"), Some(&json!(true)));
    assert_eq!(
        result_json.pointer("/customer/fromMiddle"),
        Some(&json!(true))
    );
    assert_eq!(
        result_json.pointer("/customer/fromEntry"),
        Some(&json!(true))
    );
}

#[tokio::test]
async fn cycle_terminates() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("a", make_policy_content(importing_policy_json("b", "aOut")));
    loader.add("b", make_policy_content(importing_policy_json("a", "bOut")));
    let engine = engine_with(loader);

    let result = engine
        .evaluate("a", json!({ "customer": { "age": 25 } }).into())
        .await;
    let _ = result;
}

#[tokio::test]
async fn self_import_terminates() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add(
        "a",
        make_policy_content(importing_policy_json("a", "selfOut")),
    );
    let engine = engine_with(loader);

    let result = engine
        .evaluate("a", json!({ "customer": { "age": 25 } }).into())
        .await;
    let _ = result;
}

#[tokio::test]
async fn policy_importing_graph_fails() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add(
        "entry",
        make_policy_content(importing_policy_json("g", "fromEntry")),
    );
    loader.add("g", load_test_data("table.json"));
    let engine = engine_with(loader);

    let err = engine
        .evaluate("entry", json!({ "customer": { "age": 25 } }).into())
        .await
        .expect_err("policy importing a graph should fail");

    match *err {
        EvaluationError::ContentKindMismatch { expected, got, .. } => {
            assert_eq!(expected, "policy");
            assert_eq!(got, "graph");
        }
        other => panic!("expected ContentKindMismatch, got: {other:?}"),
    }
}

#[tokio::test]
async fn missing_import_is_loader_error() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add(
        "entry",
        make_policy_content(importing_policy_json("does-not-exist", "fromEntry")),
    );
    let engine = engine_with(loader);

    let err = engine
        .evaluate("entry", json!({ "customer": { "age": 25 } }).into())
        .await
        .expect_err("missing import should surface loader error");

    assert!(
        matches!(*err, EvaluationError::LoaderError(_)),
        "expected LoaderError, got: {err:?}"
    );
}

fn assert_string_trace(value: serde_json::Value) {
    let trace = value.pointer("/trace").expect("trace key present");
    let trace_str = trace.as_str().expect("trace must serialize as a string");
    let parsed: serde_json::Value = serde_json::from_str(trace_str).expect("trace string is JSON");
    assert!(parsed.pointer("/properties").is_some());
    assert!(parsed.pointer("/executions").is_some());
}

#[tokio::test]
async fn serialized_string_trace_for_policies() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("policy", make_policy_content(simple_policy_json()));
    let engine = engine_with(loader);

    let options = EvaluationSerializedOptions {
        trace: EvaluationTraceKind::String,
        ..Default::default()
    };
    let lazy = engine
        .evaluate_serialized(
            "policy",
            json!({ "customer": { "age": 30 } }).into(),
            options,
        )
        .await
        .expect("evaluate ok");
    assert_string_trace(lazy);

    engine.compile();
    let precompiled = engine
        .evaluate_serialized(
            "policy",
            json!({ "customer": { "age": 30 } }).into(),
            options,
        )
        .await
        .expect("evaluate ok");
    assert_string_trace(precompiled);
}

#[tokio::test]
async fn diamond_imports_load_each_key_once() {
    let mut contents: HashMap<String, Arc<DecisionContent>> = HashMap::new();
    contents.insert(
        "entry".into(),
        Arc::new(make_policy_content(json!({
            "imports": ["a", "b"],
            "blocks": [
                {
                    "id": "assert-entry",
                    "type": "assertion",
                    "props": {
                        "data": {
                            "output": "customer.fromEntry",
                            "conditions": [
                                { "id": "c1", "expression": "customer.age >= 18", "operator": "and", "depth": 0 }
                            ]
                        }
                    }
                }
            ]
        }))),
    );
    contents.insert(
        "a".into(),
        Arc::new(make_policy_content(importing_policy_json("base", "fromA"))),
    );
    contents.insert(
        "b".into(),
        Arc::new(make_policy_content(importing_policy_json("base", "fromB"))),
    );
    contents.insert(
        "base".into(),
        Arc::new(make_policy_content(simple_policy_json())),
    );

    let counts: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let counts_in_loader = counts.clone();
    let engine = DecisionEngine::default().with_closure_loader(move |key| {
        let counts = counts_in_loader.clone();
        let content = contents.get(&key).cloned();
        async move {
            *counts.lock().unwrap().entry(key.clone()).or_insert(0) += 1;
            content.ok_or_else(|| LoaderError::NotFound(key))
        }
    });

    engine
        .evaluate("entry", json!({ "customer": { "age": 30 } }).into())
        .await
        .expect("evaluate ok");

    let counts = counts.lock().unwrap();
    for key in ["entry", "a", "b", "base"] {
        assert_eq!(
            counts.get(key),
            Some(&1),
            "key {key} should load exactly once, got {counts:?}"
        );
    }
}

#[tokio::test]
async fn trace_serializes_for_policies() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("policy", make_policy_content(simple_policy_json()));
    let engine = engine_with(loader);

    let result = engine
        .evaluate_with_opts(
            "policy",
            json!({ "customer": { "age": 30 } }).into(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .expect("evaluate ok");

    assert!(result.trace.is_some(), "trace requested but None returned");
    let serialized = serde_json::to_value(&result).expect("serialize ok");
    let trace = serialized.pointer("/trace").expect("trace key present");
    assert!(
        trace.pointer("/properties").is_some(),
        "policy trace should serialize with `properties` field"
    );
    assert!(
        trace.pointer("/executions").is_some(),
        "policy trace should serialize with `executions` field"
    );
}

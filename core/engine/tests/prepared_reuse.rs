use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::DecisionEngine;

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn child(value: i64) -> DecisionContent {
    document(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            { "id": "e", "name": "e", "type": "expressionNode", "content": {
                "expressions": [{ "id": "x", "key": "y", "value": format!("x + {value}") }]
            } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "a", "sourceId": "in", "targetId": "e" },
            { "id": "b", "sourceId": "e", "targetId": "out" }
        ]
    }))
}

fn parent() -> DecisionContent {
    document(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            { "id": "d", "name": "d", "type": "decisionNode", "content": { "key": "child" } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "a", "sourceId": "in", "targetId": "d" },
            { "id": "b", "sourceId": "d", "targetId": "out" }
        ]
    }))
}

#[tokio::test]
async fn graphs_and_sub_decisions_compile_once_while_the_loader_returns_the_same_content() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("parent", parent());
    loader.add("child", child(1));
    let engine = DecisionEngine::default().with_loader(loader.clone());
    let run = || async {
        engine
            .evaluate("parent", json!({ "x": 1 }).into())
            .await
            .expect("evaluates")
            .result
            .to_value()
    };

    assert_eq!(run().await, json!({ "y": 2 }));
    assert_eq!(run().await, json!({ "y": 2 }));
    let warm = zen_engine::diagnostics::compilations();
    for _ in 0..3 {
        assert_eq!(run().await, json!({ "y": 2 }));
    }
    assert_eq!(zen_engine::diagnostics::compilations(), warm);

    loader.add("child", child(10));
    assert_eq!(run().await, json!({ "y": 11 }));
    let changed = zen_engine::diagnostics::compilations();
    assert!(changed > warm);
    for _ in 0..3 {
        assert_eq!(run().await, json!({ "y": 11 }));
    }
    assert_eq!(zen_engine::diagnostics::compilations(), changed);
}

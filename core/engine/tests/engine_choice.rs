mod compiled_default;

use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::{Decision, DecisionEngine, EvaluationSerializedOptions};
use zen_expression::variable::Variable;

const KEY: &str = "throws.json";

fn content() -> DecisionContent {
    let node = |id: &str, kind: &str, content: Value| json!({ "id": id, "name": id, "type": kind, "content": content });
    let source = "export const handler = async () => { throw { code: 7, reason: 'object' }; };";
    serde_json::from_value(json!({
        "nodes": [
            node("input", "inputNode", json!({})),
            node("function", "functionNode", json!({ "version": "v2", "source": source })),
            node("output", "outputNode", json!({})),
        ],
        "edges": [
            { "id": "e1", "sourceId": "input", "targetId": "function" },
            { "id": "e2", "sourceId": "function", "targetId": "output" },
        ]
    }))
    .expect("valid decision content")
}

fn engine() -> DecisionEngine {
    let loader = Arc::new(MemoryLoader::default());
    loader.add(KEY, content());
    DecisionEngine::default().with_loader(loader)
}

fn decision() -> Decision {
    match content() {
        DecisionContent::Graph(graph) => Decision::from(graph),
        DecisionContent::Policy(_) => unreachable!("graph content"),
    }
}

fn input() -> Variable {
    json!({ "value": 1 }).into()
}

fn unaddressed(text: &str) -> String {
    match text.split_once("(0x") {
        Some((head, tail)) => match tail.split_once(')') {
            Some((_, rest)) => format!("{head}(address){}", unaddressed(rest)),
            None => text.to_string(),
        },
        None => text.to_string(),
    }
}

fn error<T>(result: Result<T, Box<zen_engine::EvaluationError>>) -> String {
    match result {
        Ok(_) => panic!("the function throws"),
        Err(error) => unaddressed(&error.to_string()),
    }
}

fn serialized(result: Result<Value, Value>) -> String {
    match result {
        Ok(_) => panic!("the function throws"),
        Err(error) => unaddressed(&error.to_string()),
    }
}

#[tokio::test]
async fn an_interpreted_engine_stays_interpreted_while_the_flag_is_on() {
    let release = error(decision().interpreted().evaluate(input()).await);
    let compiled = error(decision().compiled_engine().evaluate(input()).await);
    assert_ne!(release, compiled, "the engines must be distinguishable");

    let interpreted = engine().interpreted();
    assert_eq!(error(interpreted.evaluate(KEY, input()).await), release);
    assert_eq!(error(engine().evaluate(KEY, input()).await), compiled);

    let loaded = interpreted
        .get_decision(KEY)
        .await
        .expect("loads")
        .expect("graph");
    assert_eq!(error(loaded.evaluate(input()).await), release);
}

#[tokio::test]
async fn an_interpreted_engine_serializes_through_the_release_engine() {
    let options = EvaluationSerializedOptions::default();
    let release = serialized(
        decision()
            .interpreted()
            .evaluate_serialized(input(), options)
            .await,
    );
    let compiled = serialized(
        decision()
            .compiled_engine()
            .evaluate_serialized(input(), options)
            .await,
    );
    assert_ne!(release, compiled, "the engines must be distinguishable");
    assert_eq!(
        serialized(
            engine()
                .interpreted()
                .evaluate_serialized(KEY, input(), options)
                .await
        ),
        release
    );
}

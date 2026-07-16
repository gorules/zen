use crate::support::{create_fs_loader, load_test_data};
use serde_json::json;
use std::ops::Deref;
use std::sync::Arc;
use tokio::runtime::Builder;
use zen_engine::{Decision, DecisionGraphValidationError, EvaluationError, EvaluationOptions};

mod support;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_from_content() {
    let table_content = load_test_data("table.json");
    let decision = Decision::from(table_content);

    let context = json!({ "input": 5 });
    let result = decision.evaluate(context.into()).await;

    assert_eq!(result.unwrap().result, json!({"output": 0}).into());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_from_content_recursive() {
    let recursive_content = load_test_data("recursive-table1.json");
    let decision = Decision::from(recursive_content);

    let context = json!({});
    let result = decision.evaluate(context.clone().into()).await;
    match result.unwrap_err().deref() {
        EvaluationError::NodeError {
            node_id, source, ..
        } => {
            assert_eq!(node_id.deref(), "0b8dcf6b-fc04-47cb-bf82-bda764e6c09b");
            assert!(source.to_string().contains("Loader failed"));
        }
        _ => assert!(false, "Depth limit not exceeded"),
    }

    let with_loader = decision.with_loader(Arc::new(create_fs_loader()));
    let new_result = with_loader.evaluate(context.clone().into()).await;
    match new_result.unwrap_err().deref() {
        EvaluationError::NodeError { source, .. } => {
            assert_eq!(source.to_string(), "Depth limit exceeded")
        }
        _ => assert!(false, "Depth limit not exceeded"),
    }
}

#[test]
fn decision_expression_node() {
    let rt = Builder::new_current_thread().build().unwrap();
    let decision = Decision::from(load_test_data("expression.json"));
    let context = json!({
        "numbers": [1, 5, 15, 25],
        "firstName": "John",
        "lastName": "Doe"
    });

    let result = rt.block_on(decision.evaluate(context.into()));
    assert_eq!(
        result.unwrap().result,
        json!({
            "largeNumbers": [15, 25],
            "smallNumbers": [1, 5],
            "fullName": "John Doe",
            "deep": {
                "nested": {
                    "sum": 46
                }
            }
        })
        .into()
    )
}

#[test]
fn decision_validation() {
    let cyclic_decision = Decision::from(load_test_data("error-cyclic.json"));
    let cyclic_error = cyclic_decision.validate().unwrap_err();
    assert!(matches!(
        cyclic_error,
        DecisionGraphValidationError::CyclicGraph
    ));

    let missing_input_decision = Decision::from(load_test_data("error-missing-input.json"));
    let missing_input_error = missing_input_decision.validate().unwrap_err();
    assert!(matches!(
        missing_input_error,
        DecisionGraphValidationError::InvalidInputCount(_)
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn typescript_function_node_is_stripped_and_evaluated() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "fn", "name": "fn", "type": "functionNode",
                "content": {
                    "source": "interface Input { age: number }\nexport const handler = async (input: Input): Promise<{ total: number }> => {\n  return { total: input.age * 2 };\n};"
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "fn" },
            { "id": "e2", "sourceId": "fn", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let result = decision
        .evaluate(json!({ "age": 21 }).into())
        .await
        .unwrap();
    assert_eq!(result.result, json!({ "total": 42 }).into());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn typescript_enum_function_node_is_transformed_and_evaluated() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "fn", "name": "fn", "type": "functionNode",
                "content": {
                    "source": "enum Tier { Gold = 'gold', Basic = 'basic' }\nexport const handler = async (input: { vip: boolean }) => ({ tier: input.vip ? Tier.Gold : Tier.Basic });"
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "fn" },
            { "id": "e2", "sourceId": "fn", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let result = decision
        .evaluate(json!({ "vip": true }).into())
        .await
        .unwrap();
    assert_eq!(result.result, json!({ "tier": "gold" }).into());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_first_hit_trace_matches_untraced() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i1", "name": "Age", "field": "age" }],
                    "outputs": [{ "id": "o1", "name": "Result", "field": "result" }],
                    "rules": [
                        { "_id": "r1", "i1": "> 10", "o1": "len(age)" },
                        { "_id": "r2", "i1": "", "o1": "'fallback'" }
                    ]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "dt" },
            { "id": "e2", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let untraced = decision
        .evaluate(json!({ "age": 21 }).into())
        .await
        .unwrap();
    let traced = decision
        .evaluate_with_opts(
            json!({ "age": 21 }).into(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(untraced.result, json!({ "result": "fallback" }).into());
    assert_eq!(traced.result, untraced.result);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_missing_cell_key_is_row_miss() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i1", "name": "Age", "field": "age" }],
                    "outputs": [{ "id": "o1", "name": "Result", "field": "result" }],
                    "rules": [
                        { "_id": "r1", "o1": "'hit'" }
                    ]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "dt" },
            { "id": "e2", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let result = decision.evaluate(json!({ "age": 1 }).into()).await.unwrap();
    assert_eq!(result.result, json!({}).into());
}

use crate::support::{create_fs_loader, load_test_data};
use serde_json::json;
use std::ops::Deref;
use std::sync::Arc;
use tokio::runtime::Builder;
use zen_engine::{
    Decision, DecisionGraphValidationError, EvaluationError, EvaluationOptions, Variable,
};

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
async fn decision_table_missing_cell_key_is_treated_as_empty() {
    // Editors may store sparse rules (only the cells that were filled in).
    // A missing input cell key behaves like an empty cell ("any") and a
    // missing output cell key simply writes nothing for that column.
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i1", "name": "Age", "field": "age" }],
                    "outputs": [
                        { "id": "o1", "name": "Result", "field": "result" },
                        { "id": "o2", "name": "Extra", "field": "extra" }
                    ],
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
    assert_eq!(result.result, json!({ "result": "hit" }).into());

    let traced = decision
        .evaluate_with_opts(
            json!({ "age": 1 }).into(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(traced.result, result.result);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_column_collect_from_file() {
    let decision = Decision::from(load_test_data("table-collect-columns.json"));

    let result = decision
        .evaluate(json!({ "customer": { "age": 70 } }).into())
        .await
        .unwrap();
    assert_eq!(
        result.result,
        json!({
            "customer": {
                "tier": "senior",
                "tags": ["senior-discount", "adult", "age-verified", "customer"]
            }
        })
        .into()
    );

    let minor = decision
        .evaluate(json!({ "customer": { "age": 12 } }).into())
        .await
        .unwrap();
    assert_eq!(
        minor.result,
        json!({ "customer": { "tags": ["customer"] } }).into()
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_first_hit_column_collect() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i1", "name": "Age", "field": "age" }],
                    "outputs": [
                        { "id": "o1", "name": "Tier", "field": "customer.tier" },
                        { "id": "o2", "name": "Tags", "field": "customer.tags[]" }
                    ],
                    "rules": [
                        { "_id": "r1", "i1": "> 100", "o1": "'unreachable'", "o2": "'unreachable'" },
                        { "_id": "r2", "i1": "> 10", "o1": "'gold'", "o2": "'adult'" },
                        { "_id": "r3", "i1": "> 18", "o1": "'silver'", "o2": "'grown-up'" },
                        { "_id": "r4", "i1": "> 30", "o1": "'bronze'", "o2": "" },
                        { "_id": "r5", "i1": "", "o1": "", "o2": "'anyone'" }
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
        .evaluate(json!({ "age": 35 }).into())
        .await
        .unwrap();
    assert_eq!(
        untraced.result,
        json!({
            "customer": {
                "tier": "gold",
                "tags": ["adult", "grown-up", "anyone"]
            }
        })
        .into()
    );

    let traced = decision
        .evaluate_with_opts(
            json!({ "age": 35 }).into(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(traced.result, untraced.result);

    let fallback_only = decision
        .evaluate(json!({ "age": "not a number" }).into())
        .await
        .unwrap();
    assert_eq!(
        fallback_only.result,
        json!({ "customer": { "tags": ["anyone"] } }).into()
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn decision_table_collect_rows_do_not_alias() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "collect",
                    "inputs": [{ "id": "i1", "name": "Name", "field": "person.name" }],
                    "outputs": [
                        { "id": "o1", "name": "Person", "field": "person" },
                        { "id": "o2", "name": "Age", "field": "person.age" }
                    ],
                    "rules": [
                        { "_id": "r1", "i1": "", "o1": "person", "o2": "2" },
                        { "_id": "r2", "i1": "", "o1": "person", "o2": "3" },
                        { "_id": "r3", "i1": "", "o1": "person", "o2": "4" }
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

    let result = decision
        .evaluate(json!({ "person": { "name": "Ann" } }).into())
        .await
        .unwrap();
    assert_eq!(
        result.result,
        json!([
            { "person": { "name": "Ann", "age": 2 } },
            { "person": { "name": "Ann", "age": 3 } },
            { "person": { "name": "Ann", "age": 4 } }
        ])
        .into()
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn node_handlers_do_not_mutate_aliased_inputs() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "e1", "name": "e1", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x1", "key": "first", "value": "a + 1" }]
                }
            },
            {
                "id": "sw", "name": "sw", "type": "switchNode",
                "content": {
                    "hitPolicy": "first",
                    "statements": [{ "id": "s1", "condition": "" }]
                }
            },
            {
                "id": "e2", "name": "e2", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x2", "key": "second", "value": "first + 1" }]
                }
            },
            {
                "id": "dt", "name": "dt", "type": "decisionTableNode",
                "content": {
                    "passThrough": true,
                    "hitPolicy": "first",
                    "inputs": [{ "id": "i1", "name": "A", "field": "a" }],
                    "outputs": [{ "id": "o1", "name": "Result", "field": "dtResult" }],
                    "rules": [{ "_id": "r1", "i1": "", "o1": "'hit'" }]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "ed1", "sourceId": "in", "targetId": "e1" },
            { "id": "ed2", "sourceId": "e1", "targetId": "sw" },
            { "id": "ed3", "sourceId": "sw", "targetId": "e2", "sourceHandle": "s1" },
            { "id": "ed4", "sourceId": "e2", "targetId": "dt" },
            { "id": "ed5", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let input: Variable = json!({ "a": 1, "nested": { "k": "v" } }).into();
    let snapshot = input.to_value();

    let response = decision
        .evaluate_with_opts(
            input.clone(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(input.to_value(), snapshot);
    assert_eq!(
        response.result,
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2, "second": 3, "dtResult": "hit" })
            .into()
    );

    let trace = response.trace.unwrap().into_graph().unwrap();
    let output_of = |id: &str| trace.get(id).unwrap().output.clone();

    assert_eq!(
        output_of("in"),
        json!({ "a": 1, "nested": { "k": "v" } }).into()
    );
    assert_eq!(
        output_of("e1"),
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2 }).into()
    );
    assert_eq!(
        output_of("sw"),
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2 }).into()
    );
    assert_eq!(
        output_of("e2"),
        json!({ "a": 1, "nested": { "k": "v" }, "first": 2, "second": 3 }).into()
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn merged_inputs_do_not_mutate_parent_node_outputs() {
    let content = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            {
                "id": "pa", "name": "pa", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x1", "key": "pa", "value": "10" }]
                }
            },
            {
                "id": "pb", "name": "pb", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x2", "key": "pb", "value": "20" }]
                }
            },
            {
                "id": "join", "name": "join", "type": "expressionNode",
                "content": {
                    "passThrough": true,
                    "expressions": [{ "id": "x3", "key": "sum", "value": "pa + pb" }]
                }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "ed1", "sourceId": "in", "targetId": "pa" },
            { "id": "ed2", "sourceId": "in", "targetId": "pb" },
            { "id": "ed3", "sourceId": "pa", "targetId": "join" },
            { "id": "ed4", "sourceId": "pb", "targetId": "join" },
            { "id": "ed5", "sourceId": "join", "targetId": "out" }
        ]
    }))
    .unwrap();
    let decision = Decision::from(Arc::new(content));

    let input: Variable = json!({ "a": 1 }).into();
    let snapshot = input.to_value();

    let response = decision
        .evaluate_with_opts(
            input.clone(),
            EvaluationOptions {
                trace: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(input.to_value(), snapshot);
    assert_eq!(
        response.result,
        json!({ "a": 1, "pa": 10, "pb": 20, "sum": 30 }).into()
    );

    let trace = response.trace.unwrap().into_graph().unwrap();
    let output_of = |id: &str| trace.get(id).unwrap().output.clone();

    assert_eq!(output_of("in"), json!({ "a": 1 }).into());
    assert_eq!(output_of("pa"), json!({ "a": 1, "pa": 10 }).into());
    assert_eq!(output_of("pb"), json!({ "a": 1, "pb": 20 }).into());
}

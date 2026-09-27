use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::policy::{EvaluateRequest, EvaluationError, Workspace};
use zen_expression::variable::Variable;

fn workspace() -> Workspace {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "order",
                "properties": [
                    { "id": "p1", "name": "createdAt", "type": "date", "array": false, "optional": false },
                    { "id": "p2", "name": "total", "type": "number", "array": false, "optional": false },
                    { "id": "p3", "name": "note", "type": "string", "array": false, "optional": true },
                    { "id": "p4", "name": "buyer", "type": "relationship", "target": "customer", "array": false, "optional": true }
                ]
            }}},
            { "id": "dm2", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "c1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            }}},
            { "id": "e1", "type": "expression", "props": { "data": {
                "key": "order.recent", "value": "order.createdAt > d('2020-01-01')"
            }}},
            { "id": "e2", "type": "expression", "props": { "data": {
                "key": "order.big", "value": "order.total > 100"
            }}},
            { "id": "e3", "type": "expression", "props": { "data": {
                "key": "order.noted", "value": "order.note != null"
            }}},
            { "id": "e4", "type": "expression", "props": { "data": {
                "key": "order.adultBuyer", "value": "order.buyer != null and order.buyer.age >= 18"
            }}}
        ]
    });
    let mut ws = Workspace::new();
    ws.set_document("p", serde_json::from_value(doc).unwrap());
    ws
}

fn evaluate(order: Value, goals: &[&str]) -> Result<Value, EvaluationError> {
    workspace()
        .evaluate(&EvaluateRequest {
            policy_path: Arc::from("p"),
            input: Variable::from(json!({ "order": order })),
            goals: goals.iter().map(|g| Arc::from(*g)).collect(),
            trace: false,
        })
        .map(|result| serde_json::to_value(&result.output).unwrap())
}

fn missing(result: Result<Value, EvaluationError>) -> Vec<String> {
    match result {
        Err(EvaluationError::MissingRequiredInputs { missing, .. }) => {
            missing.iter().map(|m| m.to_string()).collect()
        }
        other => panic!("expected MissingRequiredInputs, got {other:?}"),
    }
}

#[test]
fn required_inputs_must_be_present_and_not_null() {
    assert_eq!(
        missing(evaluate(json!({ "createdAt": null, "total": 5 }), &[])),
        vec!["order.createdAt"]
    );
    assert_eq!(
        missing(evaluate(json!({ "total": 5 }), &[])),
        vec!["order.createdAt"]
    );
    assert_eq!(
        missing(evaluate(
            json!({ "createdAt": "2021-05-01", "total": null }),
            &[]
        )),
        vec!["order.total"]
    );
    assert_eq!(
        missing(evaluate(
            json!({ "createdAt": null, "total": 5 }),
            &["order.recent"]
        )),
        vec!["order.createdAt"]
    );
}

#[test]
fn optional_inputs_may_be_null_or_missing() {
    for order in [
        json!({ "createdAt": "2021-05-01", "total": 500, "note": null, "buyer": null }),
        json!({ "createdAt": "2021-05-01", "total": 500 }),
    ] {
        let out = evaluate(order, &[]).unwrap();
        assert_eq!(out["order"]["recent"], json!(true));
        assert_eq!(out["order"]["big"], json!(true));
        assert_eq!(out["order"]["noted"], json!(false));
        assert_eq!(out["order"]["adultBuyer"], json!(false));
    }
}

#[test]
fn goals_only_require_their_own_inputs() {
    let out = evaluate(json!({ "total": 500 }), &["order.big"]).unwrap();
    assert_eq!(out["order"]["big"], json!(true));
}

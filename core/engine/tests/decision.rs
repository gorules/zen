use crate::support::{benchmark, create_fs_loader, load_test_data};
use serde_json::json;
use std::ops::Deref;
use std::sync::Arc;
use tokio::runtime::Builder;
use zen_engine::{Decision, DecisionGraphValidationError, EvaluationError};

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
fn decision_expression_node_cmp() {
    let times = 10_000;
    let rt = Builder::new_current_thread().build().unwrap();
    let mut decision = Decision::from(load_test_data("expression.json"));
    let context = json!({
        "numbers": [1, 5, 15, 25],
        "firstName": "John",
        "lastName": "Doe"
    });

    let (_, st_dur) = benchmark("Decision Standard", times, None, false, || {
        let context = context.clone();
        let r = rt.block_on(decision.evaluate(context.into()));
        r
    });
    decision.compile();
    let (result, pc_dur) = benchmark("!Decision PreCompiled", times, Some(st_dur), false, || {
        let context = context.clone();
        let r = rt.block_on(decision.evaluate(context.into()));
        r
    });
    assert!(pc_dur < st_dur);
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

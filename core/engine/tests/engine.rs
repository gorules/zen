use crate::support::{create_fs_loader, load_test_data};
use serde_json::json;
use std::ops::Deref;
use std::sync::Arc;

use zen_engine::loader::{LoaderError, MemoryLoader};
use zen_engine::{DecisionEngine, EvaluationError, EvaluationOptions};

mod support;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_memory_loader() {
    let memory_loader = Arc::new(MemoryLoader::default());
    memory_loader.add("table", load_test_data("table.json"));
    memory_loader.add("function", load_test_data("function.json"));

    let engine = DecisionEngine::new_arc(memory_loader.clone());
    let table = engine.evaluate("table", &json!({ "input": 12 })).await;
    let function = engine.evaluate("function", &json!({ "input": 12 })).await;

    memory_loader.remove("function");
    let not_found = engine.evaluate("function", &json!({})).await;

    assert_eq!(table.unwrap().result, json!({"output": 10}));
    assert_eq!(function.unwrap().result, json!({"output": 24}));
    assert_eq!(not_found.unwrap_err().to_string(), "Loader error");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_filesystem_loader() {
    let engine = DecisionEngine::new(create_fs_loader());
    let table = engine.evaluate("table.json", &json!({ "input": 12 })).await;
    let function = engine
        .evaluate("function.json", &json!({ "input": 12 }))
        .await;
    let not_found = engine.evaluate("invalid_file", &json!({})).await;

    assert_eq!(table.unwrap().result, json!({"output": 10}));
    assert_eq!(function.unwrap().result, json!({"output": 24}));
    assert_eq!(not_found.unwrap_err().to_string(), "Loader error");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_closure_loader() {
    let engine = DecisionEngine::async_loader(|key| {
        // TODO: Improve once async closures become stable in Rust
        let mv_key = key.to_string();

        async move {
            match mv_key.as_str() {
                "function" => Ok(Arc::new(load_test_data("function.json"))),
                "table" => Ok(Arc::new(load_test_data("table.json"))),
                _ => Err(LoaderError::NotFound(mv_key).into()),
            }
        }
    });

    let table = engine.evaluate("table", &json!({ "input": 12 })).await;
    let function = engine.evaluate("function", &json!({ "input": 12 })).await;
    let not_found = engine.evaluate("invalid_file", &json!({})).await;

    assert_eq!(table.unwrap().result, json!({"output": 10}));
    assert_eq!(function.unwrap().result, json!({"output": 24}));
    assert_eq!(not_found.unwrap_err().to_string(), "Loader error");
}

#[tokio::test]
async fn engine_noop_loader() {
    // Default engine is noop
    let engine = DecisionEngine::default();
    let result = engine.evaluate("any.json", &json!({})).await;

    assert_eq!(result.unwrap_err().to_string(), "Loader error");
}

#[tokio::test]
async fn engine_get_decision() {
    let engine = DecisionEngine::new(create_fs_loader());

    assert!(engine.get_decision("table.json").await.is_ok());
    assert!(engine.get_decision("any.json").await.is_err());
}

#[tokio::test]
async fn engine_create_decision() {
    let engine = DecisionEngine::default();
    engine.create_decision(load_test_data("table.json").into());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_errors() {
    let engine = DecisionEngine::new(create_fs_loader());

    let infinite_fn = engine.evaluate("infinite-function.json", &json!({})).await;
    match infinite_fn.unwrap_err().deref() {
        EvaluationError::NodeError(e) => {
            assert_eq!(e.node_id, "e0fd96d0-44dc-4f0e-b825-06e56b442d78");
            assert_eq!(e.source.to_string(), "Timeout exceeded");
        }
        _ => assert!(false, "Wrong error type"),
    }

    let recursive = engine.evaluate("recursive-table1.json", &json!({})).await;
    match recursive.unwrap_err().deref() {
        EvaluationError::NodeError(e) => {
            assert_eq!(e.source.to_string(), "Depth limit exceeded")
        }
        _ => assert!(false, "Depth limit not exceeded"),
    }
}

#[tokio::test]
async fn engine_with_trace() {
    let engine = DecisionEngine::new(create_fs_loader());

    let table_r = engine.evaluate("table.json", &json!({ "input": 12 })).await;
    let table_opt_r = engine
        .evaluate_with_opts(
            "table.json",
            &json!({ "input": 12 }),
            EvaluationOptions {
                trace: Some(true),
                max_depth: None,
            },
        )
        .await;

    let table = table_r.unwrap();
    let table_opt = table_opt_r.unwrap();

    assert!(table.trace.is_none());
    assert!(table_opt.trace.is_some());

    let trace = table_opt.trace.unwrap();
    assert_eq!(trace.len(), 3); // trace for each node
}

use std::fs;
use std::io::Read;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};

use zen_engine::loader::{LoaderError, MemoryLoader};
use zen_engine::model::{DecisionContent, DecisionNodeKind};
use zen_engine::{DecisionEngine, EvaluationError, EvaluationOptions};

use crate::support::{create_fs_loader, load_raw_test_data, load_test_data, test_data_root};

mod support;

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_memory_loader() {
    let memory_loader = Arc::new(MemoryLoader::default());
    memory_loader.add("table", load_test_data("table.json"));
    memory_loader.add("function", load_test_data("function.json"));

    let engine = DecisionEngine::default().with_loader(memory_loader.clone());
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
    let engine = DecisionEngine::default().with_loader(create_fs_loader().into());
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
    let engine = DecisionEngine::default().with_closure_loader(|key| async {
        match key.as_str() {
            "function" => Ok(Arc::new(load_test_data("function.json"))),
            "table" => Ok(Arc::new(load_test_data("table.json"))),
            _ => Err(LoaderError::NotFound(key).into()),
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
    let engine = DecisionEngine::default().with_loader(create_fs_loader().into());

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
    let engine = DecisionEngine::default().with_loader(create_fs_loader().into());

    let infinite_fn = engine.evaluate("infinite-function.json", &json!({})).await;
    match infinite_fn.unwrap_err().deref() {
        EvaluationError::NodeError(e) => {
            assert_eq!(e.node_id, "e0fd96d0-44dc-4f0e-b825-06e56b442d78");
            assert!(e.source.to_string().contains("interrupted"));
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
    let engine = DecisionEngine::default().with_loader(create_fs_loader().into());

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

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_function_imports() {
    let mut function_content = load_test_data("function.json");

    let imports_js_path = Path::new("js").join("imports.js");
    let mut replace_buffer = load_raw_test_data(imports_js_path.to_str().unwrap());
    let mut replace_data = String::new();
    replace_buffer.read_to_string(&mut replace_data).unwrap();

    function_content.nodes.iter_mut().for_each(|node| {
        if let DecisionNodeKind::FunctionNode { content } = &mut node.kind {
            let _ = std::mem::replace(content, replace_data.clone());
        }
    });

    let decision = DecisionEngine::default().create_decision(function_content.into());
    let response = decision.evaluate(&json!({})).await.unwrap();

    #[derive(Deserialize, Debug)]
    #[serde(rename_all = "camelCase")]
    struct GraphResult {
        bigjs_tests: Vec<bool>,
        bigjs_valid: bool,
        dayjs_valid: bool,
        moment_valid: bool,
    }

    let result = serde_json::from_value::<GraphResult>(response.result).unwrap();

    assert!(result.bigjs_tests.iter().all(|v| *v));
    assert!(result.bigjs_valid);
    assert!(result.dayjs_valid);
    assert!(result.moment_valid);
}

#[tokio::test]
async fn engine_switch_node() {
    let engine = DecisionEngine::default().with_loader(create_fs_loader().into());

    let switch_node_r = engine
        .evaluate("switch-node.json", &json!({ "color": "yellow" }))
        .await;

    let table = switch_node_r.unwrap();
    println!("{table:?}");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn engine_graph_tests() {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestCase {
        input: Value,
        output: Value,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestData {
        tests: Vec<TestCase>,
        #[serde(flatten)]
        decision_content: DecisionContent,
    }

    let engine = DecisionEngine::default();

    let graphs_path = Path::new(test_data_root().as_str()).join("graphs");
    let file_list = std::fs::read_dir(graphs_path).unwrap();
    for maybe_file in file_list {
        let Ok(file) = maybe_file else {
            panic!("Failed to read DirEntry {maybe_file:?}");
        };

        let file_name = file.file_name().to_str().map(|s| s.to_string()).unwrap();
        let file_contents = fs::read_to_string(file.path()).expect("valid file data");
        let test_data: TestData = serde_json::from_str(&file_contents).expect("Valid JSON");

        let decision = engine.create_decision(test_data.decision_content.into());
        for test_case in test_data.tests {
            let result = decision.evaluate(&test_case.input).await.unwrap().result;
            let input = test_case.input;
            assert_eq!(
                test_case.output, result,
                "Decision file: {file_name}.\nInput:\n {input:#?}"
            );
        }
    }
}

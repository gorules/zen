use std::sync::Arc;

use serde_json::{json, Value};
use zen_engine::loader::MemoryLoader;
use zen_engine::policy::{EvaluateRequest, PolicyWorkspace, ScopeRequest, Severity};
use zen_engine::DecisionEngine;

fn expression(key: &str, value: &str) -> Value {
    json!({"id": key, "type": "expression", "props": {"data": {"key": key, "value": value}}})
}

fn dictionary(name: &str, value: &str) -> Value {
    json!({"id": name, "type": "dictionary", "props": {"data": {
        "name": name, "entries": [{"value": value, "label": value}]
    }}})
}

fn policy(imports: &[&str], blocks: Vec<Value>) -> Value {
    json!({"contentType": "policy", "imports": imports, "blocks": blocks})
}

fn siblings(duplicate: bool) -> Vec<(&'static str, Value)> {
    vec![
        ("A", policy(&[], vec![expression("base", "10")])),
        (
            "B",
            policy(
                &["A"],
                vec![
                    dictionary("choice", "approve"),
                    expression("decision", "\"approve\""),
                ],
            ),
        ),
        (
            "C",
            policy(
                &["A"],
                vec![
                    dictionary(if duplicate { "choice" } else { "otherChoice" }, "decline"),
                    expression(if duplicate { "decision" } else { "other" }, "\"decline\""),
                ],
            ),
        ),
    ]
}

fn workspace(docs: &[(&str, Value)]) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    for (path, doc) in docs {
        ws.set_policy(*path, serde_json::from_value(doc.clone()).unwrap());
    }
    ws
}

fn request(path: &str, input: Value) -> EvaluateRequest {
    EvaluateRequest {
        policy_path: Arc::from(path),
        input: input.into(),
        goals: vec![],
        trace: true,
    }
}

fn output(ws: &PolicyWorkspace, path: &str) -> Value {
    serde_json::to_value(ws.evaluate(&request(path, json!({}))).unwrap().output).unwrap()
}

#[test]
fn workspace_and_trace_only_execute_outgoing_imports() {
    let ws = workspace(&siblings(false));
    for (path, expected, count) in [
        ("A", json!({"base":10}), 1),
        ("B", json!({"base":10,"decision":"approve"}), 2),
        ("C", json!({"base":10,"other":"decline"}), 2),
    ] {
        assert_eq!(output(&ws, path), expected);
        let enhanced = ws.enhance_trace(&request(path, json!({}))).unwrap();
        assert_eq!(serde_json::to_value(enhanced.output).unwrap(), expected);
        assert_eq!(enhanced.trace.unwrap().executions.len(), count);
    }
}

#[tokio::test]
async fn compiled_and_lazy_engines_agree_with_sibling_duplicate_names() {
    for duplicate in [false, true] {
        let docs = siblings(duplicate);
        for precompile in [false, true] {
            let loader = Arc::new(MemoryLoader::default());
            for (path, doc) in &docs {
                loader.add(
                    *path,
                    serde_json::from_value::<zen_engine::model::DecisionContent>(doc.clone())
                        .unwrap(),
                );
            }
            let engine = DecisionEngine::default().with_loader(loader);
            if precompile {
                assert!(engine.compile().is_empty());
            }
            // Switching entries repeatedly must never reuse another entry's scope.
            for path in ["B", "C", "A", "C", "B"] {
                let result = engine.evaluate(path, json!({}).into()).await.unwrap();
                let result = serde_json::to_value(result.result).unwrap();
                let expected = match path {
                    "B" => json!({"base":10,"decision":"approve"}),
                    "C" if duplicate => json!({"base":10,"decision":"decline"}),
                    "C" => json!({"base":10,"other":"decline"}),
                    _ => json!({"base":10}),
                };
                assert_eq!(result, expected, "precompile={precompile}, entry={path}");
            }
        }
    }
}

#[test]
fn sibling_dictionaries_and_diagnostics_stay_separate() {
    let ws = workspace(&siblings(true));
    for (path, values) in [
        ("A", vec![]),
        ("B", vec!["approve"]),
        ("C", vec!["decline"]),
    ] {
        let errors: Vec<_> = ws
            .diagnostics(path)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "{path}: {errors:?}");
        let dictionaries = ws.dictionaries(&ScopeRequest::for_policy(path));
        let actual: Vec<_> = dictionaries
            .iter()
            .flat_map(|d| d.entries.iter().map(|e| e.value.as_ref()))
            .collect();
        assert_eq!(actual, values);
    }
}

#[tokio::test]
async fn explicitly_composing_conflicting_siblings_reports_errors_at_the_entry() {
    let mut docs = siblings(true);
    docs.push(("D", policy(&["B", "C"], vec![])));
    let ws = workspace(&docs);
    let diagnostics = ws.diagnostics("D");
    for code in ["DuplicateWriter", "DataModelCollision"] {
        assert!(
            diagnostics.iter().any(|d| format!("{:?}", d.code) == code),
            "{diagnostics:?}"
        );
    }
    assert!(diagnostics
        .iter()
        .all(|d| d.location.policy_path.as_ref() == "D"));
    for precompile in [false, true] {
        let loader = Arc::new(MemoryLoader::default());
        for (path, doc) in &docs {
            loader.add(
                *path,
                serde_json::from_value::<zen_engine::model::DecisionContent>(doc.clone()).unwrap(),
            );
        }
        let engine = DecisionEngine::default().with_loader(loader);
        if precompile {
            let failures = engine.compile();
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].key.as_ref(), "D");
        }
        assert!(engine.evaluate("D", json!({}).into()).await.is_err());
        assert!(engine.evaluate("B", json!({}).into()).await.is_ok());
        assert!(engine.evaluate("C", json!({}).into()).await.is_ok());
    }
}

#[test]
fn diamond_imports_execute_the_shared_dependency_once() {
    let mut docs = siblings(false);
    docs.push((
        "D",
        policy(&["B", "C"], vec![expression("total", "base + 5")]),
    ));
    let ws = workspace(&docs);
    let result = ws.enhance_trace(&request("D", json!({}))).unwrap();
    assert_eq!(
        serde_json::to_value(result.output).unwrap(),
        json!({"base":10,"decision":"approve","other":"decline","total":15})
    );
    let trace = result.trace.unwrap();
    assert_eq!(trace.executions.len(), 4);
    assert_eq!(
        trace
            .executions
            .iter()
            .filter(|e| e.block_id.as_ref() == "base")
            .count(),
        1
    );
}

#[test]
fn scope_updates_after_import_and_sibling_edits() {
    let mut ws = workspace(&siblings(false));
    assert_eq!(output(&ws, "B"), json!({"base":10,"decision":"approve"}));
    ws.set_policy(
        "C",
        serde_json::from_value(policy(&["A"], vec![expression("decision", "\"decline\"")]))
            .unwrap(),
    );
    assert_eq!(output(&ws, "B"), json!({"base":10,"decision":"approve"}));
    ws.set_policy(
        "B",
        serde_json::from_value(policy(&[], vec![expression("decision", "\"approve\"")])).unwrap(),
    );
    assert_eq!(output(&ws, "B"), json!({"decision":"approve"}));
    ws.set_policy(
        "B",
        serde_json::from_value(policy(&["C"], vec![expression("more", "base + 1")])).unwrap(),
    );
    assert_eq!(
        output(&ws, "B"),
        json!({"base":10,"decision":"decline","more":11})
    );
}

#[test]
fn unrelated_sibling_input_and_entity_scope_cannot_poison_evaluation() {
    let docs = vec![
        ("shared", policy(&[], vec![])),
        (
            "Test",
            policy(
                &["shared"],
                vec![
                    json!({"id":"dm", "type":"dataModel", "props":{"data":{"name":"person", "properties":[{"id":"age", "name":"age", "type":"number"}]}}}),
                    expression("decision", "age > 50"),
                ],
            ),
        ),
        (
            "underwriting",
            policy(
                &["shared"],
                vec![
                    json!({"id":"rule", "type":"decisionTable", "props":{"data":{
                        "hitPolicy":"first", "inputs":[], "outputs":[
                            {"id":"x", "name":"", "field":"person.result"},
                            {"id":"y", "name":"", "field":"other.result"}
                        ], "rules":[{"_id":"r1","x":"1","y":"2"}]
                    }}}),
                ],
            ),
        ),
    ];
    let ws = workspace(&docs);
    let errors: Vec<_> = ws
        .diagnostics("underwriting")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        output(&ws, "underwriting"),
        json!({"person":{"result":1},"other":{"result":2}})
    );
    assert!(ws
        .enhance_trace(&request("underwriting", json!({})))
        .is_ok());
}

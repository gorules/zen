use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EngineVersion, EvaluationOptions, Variable, ZEN_CONFIG};

const TIMEOUT_MILLIS: u64 = 150;

fn graph(source: &str) -> Arc<DecisionContent> {
    Arc::new(
        serde_json::from_value(json!({
            "nodes": [
                {"id": "in", "name": "in", "type": "inputNode"},
                {"id": "node", "name": "node", "type": "functionNode", "content": {"source": source}},
                {"id": "out", "name": "out", "type": "outputNode"}
            ],
            "edges": [
                {"id": "e1", "sourceId": "in", "targetId": "node", "type": "edge"},
                {"id": "e2", "sourceId": "node", "targetId": "out", "type": "edge"}
            ]
        }))
        .unwrap(),
    )
}

fn outcomes(source: &str) -> Vec<(Result<Value, String>, Duration)> {
    ZEN_CONFIG
        .function_timeout_millis
        .store(TIMEOUT_MILLIS, Ordering::Relaxed);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let engine = DecisionEngine::default().with_version(EngineVersion::V2);
    let mut decision = engine.create_decision(graph(source)).unwrap();
    let input = || Variable::from(json!({"x": 1}));
    let timed = |run: &dyn Fn() -> Result<Value, String>| {
        let start = Instant::now();
        let result = run();
        (result, start.elapsed())
    };
    let shown =
        |result: Result<zen_engine::DecisionGraphResponse, Box<zen_engine::EvaluationError>>| {
            result
                .map(|response| response.result.to_value())
                .map_err(|error| serde_json::to_string(&error).unwrap())
        };
    let mut found = Vec::new();
    for _ in 0..2 {
        found.push(timed(&|| {
            shown(runtime.block_on(decision.evaluate(input())))
        }));
    }
    decision.compile();
    found.push(timed(&|| {
        shown(runtime.block_on(decision.evaluate(input())))
    }));
    let start = Instant::now();
    let rows = vec![input(), input(), input()];
    let batch = runtime.block_on(decision.evaluate_batch(&rows, EvaluationOptions::default()));
    let elapsed = start.elapsed();
    found.extend(batch.into_iter().map(|row| (shown(row), elapsed)));
    found
}

fn fails_with(source: &str, message: &str, rows_in_batch: u32) {
    let limit = Duration::from_millis(TIMEOUT_MILLIS);
    for (at, (result, elapsed)) in outcomes(source).into_iter().enumerate() {
        let error = result.expect_err(&format!("{source} {at} should fail"));
        assert!(error.contains(message), "{source} {at}: {error}");
        assert!(error.contains("node"), "{source} {at}: {error}");
        let bound = limit * rows_in_batch.max(1) * 10 + Duration::from_secs(2);
        assert!(elapsed < bound, "{source} {at} took {elapsed:?}");
    }
}

#[test]
fn a_promise_that_never_settles_times_out() {
    fails_with(
        "export const handler = async (input) => new Promise(() => {});",
        "interrupted",
        3,
    );
    fails_with(
        "export const handler = (input) => new Promise(() => {});",
        "interrupted",
        3,
    );
    fails_with(
        "export const handler = async (input) => { await new Promise(() => {}); return { done: true }; };",
        "interrupted",
        3,
    );
}

#[test]
fn a_module_whose_top_level_await_never_settles_times_out() {
    fails_with(
        "await new Promise(() => {});\nexport const handler = async (input) => ({ done: true });",
        "interrupted",
        3,
    );
}

#[test]
fn a_promise_that_spins_on_microtasks_times_out() {
    fails_with(
        "export const handler = async (input) => { while (true) { await null; } };",
        "interrupted",
        3,
    );
}

#[test]
fn a_timed_out_row_takes_at_least_the_function_timeout() {
    let limit = Duration::from_millis(TIMEOUT_MILLIS);
    for (result, elapsed) in
        outcomes("export const handler = async (input) => new Promise(() => {});")
    {
        assert!(result.is_err());
        assert!(elapsed >= limit, "{elapsed:?}");
    }
}

#[test]
fn a_promise_that_settles_later_still_resolves() {
    for (result, _) in outcomes(
        "export const handler = async (input) => { await Promise.resolve(); const v = await new Promise((resolve) => Promise.resolve().then(() => resolve(input.x + 1))); return { v }; };",
    ) {
        assert_eq!(result, Ok(json!({"v": 2})));
    }
}

#[test]
fn a_cyclic_result_is_an_error() {
    fails_with(
        "export const handler = async (input) => { const o = {}; o.self = o; return o; };",
        "cyclic value",
        1,
    );
    fails_with(
        "export const handler = (input) => { const a = [1]; a.push({ a }); return { a }; };",
        "cyclic value",
        1,
    );
    fails_with(
        "export const handler = async (input) => { const o = { x: 1 }; o.later = Promise.resolve(o); return o; };",
        "cyclic value",
        1,
    );
}

#[test]
fn a_shared_but_acyclic_result_is_not_an_error() {
    for (result, _) in outcomes(
        "export const handler = async (input) => { const s = { a: 1 }; const list = [s, s]; return { x: s, y: s, list, nested: { list } }; };",
    ) {
        assert_eq!(
            result,
            Ok(json!({
                "x": {"a": 1}, "y": {"a": 1}, "list": [{"a": 1}, {"a": 1}],
                "nested": {"list": [{"a": 1}, {"a": 1}]}
            }))
        );
    }
}

#[test]
fn a_host_call_longer_than_the_timeout_still_resolves_as_on_version_1() {
    let source = "export const handler = async (input) => { await console.sleep(400); return { v: input.x + 1 }; };";
    for (result, elapsed) in outcomes(source) {
        assert_eq!(result, Ok(json!({"v": 2})));
        assert!(
            elapsed >= Duration::from_millis(TIMEOUT_MILLIS * 2),
            "{elapsed:?}"
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let old = DecisionEngine::default()
        .with_version(EngineVersion::V1)
        .create_decision(graph(source))
        .unwrap();
    let result = runtime
        .block_on(old.evaluate(Variable::from(json!({"x": 1}))))
        .map(|response| response.result.to_value())
        .map_err(|error| error.to_string());
    assert_eq!(result, Ok(json!({"v": 2})));
}

#[test]
fn a_promise_left_pending_after_a_host_call_times_out() {
    fails_with(
        "export const handler = async (input) => { await console.sleep(10); return new Promise(() => {}); };",
        "interrupted",
        3,
    );
}

use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::{DecisionContent, GraphContent};
use zen_engine::{Decision, EvaluationOptions, ZEN_CONFIG};
use zen_expression::Variable;

fn function(id: &str, source: &str) -> Value {
    json!({ "id": id, "name": id, "type": "functionNode", "content": { "version": "v2", "source": source } })
}

fn chain(nodes: Vec<Value>) -> GraphContent {
    let mut all =
        vec![json!({ "id": "input", "name": "input", "type": "inputNode", "content": {} })];
    all.extend(nodes);
    all.push(json!({ "id": "output", "name": "output", "type": "outputNode", "content": {} }));
    let edges: Vec<Value> = all
        .windows(2)
        .enumerate()
        .map(|(i, w)| json!({ "id": format!("e{i}"), "sourceId": w[0]["id"], "targetId": w[1]["id"] }))
        .collect();
    serde_json::from_value(json!({ "nodes": all, "edges": edges })).expect("graph")
}

fn outcome(result: Result<Variable, String>) -> Value {
    match result {
        Ok(value) => value.to_value(),
        Err(error) => json!({ "error": error }),
    }
}

async fn compare(decision: &Decision, inputs: &[Value]) -> Vec<Value> {
    let rows: Vec<Variable> = (0..inputs.len() * 3)
        .map(|i| Variable::from(inputs[i % inputs.len()].clone()))
        .collect();
    let mut single = Vec::new();
    for row in &rows {
        let result = decision.evaluate(row.depth_clone(64)).await;
        single.push(outcome(result.map(|r| r.result).map_err(|e| e.to_string())));
    }
    let batch: Vec<Value> = decision
        .evaluate_batch(&rows, EvaluationOptions::default())
        .await
        .into_iter()
        .map(|r| outcome(r.map(|r| r.result).map_err(|e| e.to_string())))
        .collect();
    assert_eq!(batch, single);
    batch
}

#[tokio::test]
async fn promise_and_bigint_handlers_match_single_rows_in_a_batch() {
    let source = "export const handler = async (input) => { const all = await Promise.allSettled([Promise.reject(new Error('no')), Promise.resolve(input.n)]); const first = await Promise.race([new Promise((resolve) => resolve(input.n * 2))]); return { first, settled: all.map((s) => s.status), big: Number(BigInt(input.n) * 3n) }; };";
    let decision = Decision::from(chain(vec![function("f", source)])).compiled_engine();
    let out = compare(&decision, &[json!({ "n": 1 }), json!({ "n": 2 })]).await;
    assert_eq!(
        out[1],
        json!({ "first": 4, "settled": ["rejected", "fulfilled"], "big": 6 })
    );
}

#[tokio::test]
async fn a_stateful_child_called_through_zen_evaluate_is_fresh_on_every_row() {
    let loader = Arc::new(MemoryLoader::default());
    let child: DecisionContent = serde_json::from_value(
        serde_json::to_value(chain(vec![function(
            "c",
            "let n = 0;\nexport const handler = async (input) => ({ n: ++n });",
        )]))
        .expect("child"),
    )
    .expect("child content");
    loader.add("counter", child);
    let source = "import zen from 'zen';\nexport const handler = async (input) => { const a = await zen.evaluate('counter', {}); const b = await zen.evaluate('counter', {}); return { a: a.result.n, b: b.result.n, v: input.v }; };";
    let decision = Decision::from(chain(vec![function("f", source)]))
        .with_loader(loader)
        .compiled_engine();
    let out = compare(&decision, &[json!({ "v": 1 }), json!({ "v": 2 })]).await;
    assert!(
        out.iter()
            .all(|row| row["a"] == json!(1) && row["b"] == json!(1)),
        "{out:?}"
    );
}

#[tokio::test]
async fn globals_written_by_one_row_are_not_seen_by_the_next() {
    let source = "export const handler = async (input) => { const before = globalThis.specRows ?? 0; globalThis.specRows = before + 1; return { before }; };";
    let decision = Decision::from(chain(vec![function("f", source)])).compiled_engine();
    let out = compare(&decision, &[json!({}), json!({ "x": 1 })]).await;
    assert!(out.iter().all(|row| row["before"] == json!(0)), "{out:?}");
}

#[tokio::test]
async fn a_row_that_times_out_does_not_fail_its_neighbours_in_a_batch() {
    ZEN_CONFIG
        .function_timeout_millis
        .store(100, Ordering::Relaxed);
    let source = "export const handler = async (input) => { let i = 0; while (input.spin) { i++; } return { ok: true, i }; };";
    let decision = Decision::from(chain(vec![function("f", source)])).compiled_engine();
    let rows: Vec<Variable> = [
        json!({ "spin": false }),
        json!({ "spin": true }),
        json!({ "spin": false }),
    ]
    .into_iter()
    .map(Variable::from)
    .collect();
    let out = decision
        .evaluate_batch(&rows, EvaluationOptions::default())
        .await;
    assert!(
        out[0].is_ok(),
        "{:?}",
        out[0].as_ref().err().map(|e| e.to_string())
    );
    assert!(out[1].is_err());
    assert!(
        out[2].is_ok(),
        "{:?}",
        out[2].as_ref().err().map(|e| e.to_string())
    );
}

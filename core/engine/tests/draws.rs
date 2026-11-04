mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use zen_engine::clock::{parse_instant, parse_timezone, row_seed, unrowed_draws};
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::trace::TraceFormat;
use zen_engine::{Decision, DecisionEngine, EvaluationOptions, Variable};

const NOW: &str = "2026-11-04T09:30:00.125Z";
const ROWS: usize = 7;

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn compiled() {
    zen_engine::ZEN_CONFIG
        .compiled
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

fn chain(nodes: Vec<Value>) -> Value {
    let ids: Vec<String> = nodes
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    let edges: Vec<Value> = ids
        .windows(2)
        .map(|w| json!({"id": format!("{}-{}", w[0], w[1]), "sourceId": w[0], "targetId": w[1], "type": "edge"}))
        .collect();
    json!({"nodes": nodes, "edges": edges})
}

fn input() -> Value {
    json!({"id": "in", "name": "in", "type": "inputNode"})
}

fn output() -> Value {
    json!({"id": "out", "name": "out", "type": "outputNode"})
}

fn expressions(id: &str, fields: &[(&str, &str)], extra: Value) -> Value {
    let expressions: Vec<Value> = fields
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({"id": format!("{id}{i}"), "key": key, "value": value}))
        .collect();
    let mut content = json!({"expressions": expressions});
    if let (Some(map), Some(extra)) = (content.as_object_mut(), extra.as_object()) {
        map.extend(extra.clone());
    }
    json!({"id": id, "name": id, "type": "expressionNode", "content": content})
}

fn content(graph: Value) -> Arc<DecisionContent> {
    Arc::new(serde_json::from_value(graph).unwrap())
}

fn decision(graph: Value, children: &[(&str, Value)]) -> Decision {
    compiled();
    let loader = Arc::new(MemoryLoader::default());
    for (key, child) in children {
        let child: DecisionContent = serde_json::from_value(child.clone()).unwrap();
        loader.add(*key, child);
    }
    DecisionEngine::default()
        .with_loader(loader)
        .create_decision(content(graph))
        .unwrap()
}

fn top_level() -> Decision {
    decision(
        chain(vec![
            input(),
            expressions(
                "calc",
                &[
                    ("a", "rand(1000000)"),
                    ("b", "x + rand(1000000)"),
                    ("c", "x > 2 ? rand(100) : -1"),
                ],
                json!({}),
            ),
            output(),
        ]),
        &[],
    )
}

fn closures() -> Decision {
    decision(
        chain(vec![
            input(),
            expressions(
                "calc",
                &[
                    ("m", "map(xs, rand(1000000))"),
                    ("two", "map(xs, rand(1000) * 1000 + rand(1000))"),
                    ("f", "filter(xs, rand(10) > 4)"),
                    ("some", "some(xs, rand(10) > 7)"),
                    (
                        "nested",
                        "map(filter(xs, # < 3), map(filter(xs, # < 4), rand(1000)))",
                    ),
                    ("fixed", "map([1, 2, 3, 4, 5], rand(1000000))"),
                    ("after", "rand(1000000)"),
                ],
                json!({}),
            ),
            output(),
        ]),
        &[],
    )
}

fn looped() -> Decision {
    decision(
        chain(vec![
            input(),
            expressions(
                "calc",
                &[
                    ("r", "rand(1000000)"),
                    ("s", "n + rand(1000)"),
                    ("inner", "map([1, 2], rand(1000))"),
                ],
                json!({"inputField": "items", "outputPath": "looped", "executionMode": "loop"}),
            ),
            expressions(
                "after",
                &[("z", "rand(1000000)")],
                json!({"passThrough": true}),
            ),
            output(),
        ]),
        &[],
    )
}

fn child() -> Value {
    chain(vec![
        input(),
        expressions(
            "inner",
            &[("c", "rand(1000000)"), ("d", "map([1, 2, 3], rand(1000))")],
            json!({}),
        ),
        output(),
    ])
}

fn sub_decisions() -> Decision {
    decision(
        chain(vec![
            input(),
            expressions(
                "before",
                &[("a", "rand(1000000)")],
                json!({"passThrough": true}),
            ),
            json!({"id": "call", "name": "call", "type": "decisionNode", "content": {"key": "child"}}),
            expressions(
                "after",
                &[("b", "rand(1000000)")],
                json!({"passThrough": true}),
            ),
            output(),
        ]),
        &[("child", child())],
    )
}

fn looped_sub_decisions() -> Decision {
    decision(
        chain(vec![
            input(),
            json!({"id": "call", "name": "call", "type": "decisionNode", "content": {
                "key": "child", "inputField": "items", "outputPath": "children", "executionMode": "loop"
            }}),
            expressions(
                "after",
                &[("b", "rand(1000000)")],
                json!({"passThrough": true}),
            ),
            output(),
        ]),
        &[("child", child())],
    )
}

fn functions() -> Decision {
    decision(
        chain(vec![
            input(),
            expressions(
                "before",
                &[("a", "rand(1000000)")],
                json!({"passThrough": true}),
            ),
            json!({"id": "fn", "name": "fn", "type": "functionNode", "content": {
                "source": "export const handler = async (input) => ({ ...input, r: Math.random(), q: Math.random() })"
            }}),
            json!({"id": "fn1", "name": "fn1", "type": "functionNode", "content":
                "const handler = (input) => ({ ...input, v1: Math.random() })"
            }),
            expressions(
                "after",
                &[("b", "rand(1000000)")],
                json!({"passThrough": true}),
            ),
            output(),
        ]),
        &[],
    )
}

fn tables() -> Decision {
    decision(
        chain(vec![
            input(),
            json!({"id": "dt", "name": "dt", "type": "decisionTableNode", "content": {
                "hitPolicy": "collect",
                "inputs": [{"id": "i1", "name": "x", "field": "x"}],
                "outputs": [{"id": "o1", "name": "r", "field": "r"}, {"id": "o2", "name": "k", "field": "k"}],
                "rules": [
                    {"_id": "r1", "i1": "> 1", "o1": "rand(1000000)", "o2": "1"},
                    {"_id": "r2", "i1": "", "o1": "rand(1000000)", "o2": "2"}
                ]
            }}),
            output(),
        ]),
        &[],
    )
}

fn branches() -> Decision {
    let graph = json!({
        "nodes": [
            input(),
            {"id": "sw", "name": "sw", "type": "switchNode", "content": {
                "hitPolicy": "first",
                "statements": [
                    {"id": "s1", "condition": "x > 2"},
                    {"id": "s2", "condition": ""}
                ]
            }},
            expressions(
                "high",
                &[("h", "rand(1000000)"), ("hs", "map(xs, rand(100))")],
                json!({"passThrough": true}),
            ),
            {"id": "low", "name": "low", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "passThrough": true,
                "inputs": [{"id": "i1", "name": "x", "field": "x"}],
                "outputs": [{"id": "o1", "name": "l", "field": "l"}],
                "rules": [
                    {"_id": "r1", "i1": "rand(10) > 6", "o1": "rand(1000000)"},
                    {"_id": "r2", "i1": "rand(10) > 3", "o1": "-rand(1000)"},
                    {"_id": "r3", "i1": "", "o1": "0"}
                ]
            }},
            expressions(
                "each",
                &[("e", "rand(1000) + n")],
                json!({"inputField": "items", "outputPath": "each", "executionMode": "loop", "passThrough": true}),
            ),
            output()
        ],
        "edges": [
            {"id": "e0", "sourceId": "in", "targetId": "sw", "type": "edge"},
            {"id": "e1", "sourceId": "sw", "sourceHandle": "s1", "targetId": "high", "type": "edge"},
            {"id": "e2", "sourceId": "sw", "sourceHandle": "s2", "targetId": "low", "type": "edge"},
            {"id": "e3", "sourceId": "high", "targetId": "each", "type": "edge"},
            {"id": "e4", "sourceId": "low", "targetId": "out", "type": "edge"},
            {"id": "e5", "sourceId": "each", "targetId": "out", "type": "edge"}
        ]
    });
    decision(graph, &[])
}

fn row(x: usize) -> Variable {
    let xs: Vec<usize> = (0..(3 + x * 23)).collect();
    let items: Vec<Value> = (0..(1 + x * 13 % 70)).map(|n| json!({"n": n})).collect();
    Variable::from(json!({"x": x, "xs": xs, "items": items}))
}

fn options(trace: TraceFormat, seed: u64) -> EvaluationOptions {
    EvaluationOptions {
        trace,
        now: parse_instant(NOW),
        timezone: parse_timezone("Europe/Berlin"),
        seed: Some(seed),
        ..Default::default()
    }
}

fn untimed(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, _)| k != "perf" && k != "performance" && k != "timings")
                .map(|(k, x)| (k, untimed(x)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(untimed).collect()),
        other => other,
    }
}

fn alone(d: &Decision, x: usize, seed: u64) -> (Value, Value) {
    let r = block(d.evaluate_with_opts(row(x), options(TraceFormat::V1, seed))).unwrap();
    let off = block(d.evaluate_with_opts(row(x), options(TraceFormat::Off, seed))).unwrap();
    let result = serde_json::to_value(&r.result).unwrap();
    assert_eq!(
        result,
        serde_json::to_value(&off.result).unwrap(),
        "traced and untraced single row {x}"
    );
    (result, serde_json::to_value(r.trace.unwrap()).unwrap())
}

fn rows_match_alone(name: &str, d: &Decision, base: u64) -> Vec<Value> {
    let inputs: Vec<Variable> = (0..ROWS).map(row).collect();
    let singles: Vec<(Value, Value)> = (0..ROWS).map(|x| alone(d, x, row_seed(base, x))).collect();
    for trace in [TraceFormat::V1, TraceFormat::Off] {
        let runs = zen_engine::trace::testing::graph_runs();
        let batch = block(d.evaluate_batch(&inputs, options(trace, base)));
        assert_eq!(
            zen_engine::trace::testing::graph_runs() - runs,
            1,
            "{name}: one run per batch"
        );
        for (x, result) in batch.into_iter().enumerate() {
            let result = result.unwrap();
            let (want, doc) = &singles[x];
            assert_eq!(
                &serde_json::to_value(&result.result).unwrap(),
                want,
                "{name}: batch row {x} ({trace:?})"
            );
            if let Some(t) = result.trace {
                let t = serde_json::to_value(&t).unwrap();
                assert_eq!(
                    t["seed"],
                    json!(row_seed(base, x)),
                    "{name}: seed of row {x}"
                );
                assert_eq!(untimed(t), untimed(doc.clone()), "{name}: batch trace {x}");
            }
        }
    }
    singles.into_iter().map(|(v, _)| v).collect()
}

fn row_json(x: usize) -> Value {
    serde_json::to_value(row(x)).unwrap()
}

fn columns_match_alone(name: &str, d: &Decision, base: u64, json: impl Fn(usize) -> Value) {
    let rows: Vec<Value> = (0..ROWS).map(&json).collect();
    let built = Built::nested(&rows)
        .or_else(|| Built::lists(&rows))
        .or_else(|| Built::new(&rows))
        .expect("columns");
    let columns = built.columns();
    for trace in [TraceFormat::V1, TraceFormat::Off] {
        let runs = zen_engine::trace::testing::graph_runs();
        let output = block(d.evaluate_columns(&columns, options(trace, base)));
        assert_eq!(
            zen_engine::trace::testing::graph_runs() - runs,
            1,
            "{name}: one columnar run"
        );
        for (x, row) in rows.iter().enumerate() {
            let input = Variable::from(row.clone());
            let single =
                block(d.evaluate_with_opts(input, options(TraceFormat::V1, row_seed(base, x))))
                    .unwrap();
            let got = serde_json::to_value(output.row(x)).unwrap();
            assert_eq!(
                got,
                serde_json::to_value(&single.result).unwrap(),
                "{name}: columnar row {x} ({trace:?})"
            );
            if trace == TraceFormat::V1 {
                let t: Value = serde_json::from_str(&output.trace_json(x).unwrap()).unwrap();
                let doc = serde_json::to_value(single.trace.unwrap()).unwrap();
                assert_eq!(untimed(t), untimed(doc), "{name}: columnar trace {x}");
            }
        }
    }
}

fn distinct(values: impl IntoIterator<Item = Value>) -> usize {
    let mut seen: Vec<String> = values.into_iter().map(|v| v.to_string()).collect();
    seen.sort();
    seen.dedup();
    seen.len()
}

#[test]
fn top_level_draws_follow_each_rows_stream() {
    let d = top_level();
    let rows = rows_match_alone("top level", &d, 41);
    assert_eq!(distinct(rows.iter().map(|r| r["a"].clone())), ROWS);
    columns_match_alone("top level", &d, 41, row_json);
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn closure_items_draw_independently() {
    let d = closures();
    let rows = rows_match_alone("closures", &d, 7);
    for r in &rows {
        let m = r["m"].as_array().unwrap();
        assert!(distinct(m.iter().cloned()) > m.len() / 2, "{r}");
        let fixed = r["fixed"].as_array().unwrap();
        assert!(distinct(fixed.iter().cloned()) > 2, "{r}");
        let nested = r["nested"].as_array().unwrap();
        assert!(distinct(nested.iter().flat_map(|n| n.as_array().unwrap().clone())) > 3);
    }
    assert_eq!(distinct(rows.iter().map(|r| r["fixed"].clone())), ROWS);
    let fixed = decision(
        chain(vec![
            input(),
            expressions(
                "calc",
                &[
                    ("m", "map([1, 2, 3, 4, 5, 6], rand(1000000) + x)"),
                    ("two", "map([1, 2, 3], rand(1000) * 1000 + rand(1000))"),
                    ("f", "filter([1, 2, 3, 4, 5, 6, 7, 8], rand(10) > 4)"),
                ],
                json!({}),
            ),
            output(),
        ]),
        &[],
    );
    columns_match_alone("closures", &d, 7, row_json);
    columns_match_alone("fixed closures", &fixed, 7, |x| json!({"x": x}));
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn loop_nodes_draw_per_row() {
    let d = looped();
    let rows = rows_match_alone("loops", &d, 3);
    assert_eq!(distinct(rows.iter().map(|r| r["z"].clone())), ROWS);
    let first: Vec<Value> = rows.iter().map(|r| r["looped"][0]["r"].clone()).collect();
    assert_eq!(distinct(first), ROWS);
    columns_match_alone("loops", &d, 3, row_json);
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn sub_decisions_draw_from_the_callers_row() {
    let d = sub_decisions();
    let rows = rows_match_alone("sub decisions", &d, 19);
    assert_eq!(distinct(rows.iter().map(|r| r["c"].clone())), ROWS);
    columns_match_alone("sub decisions", &d, 19, row_json);
    let d = looped_sub_decisions();
    rows_match_alone("looped sub decisions", &d, 23);
    columns_match_alone("looped sub decisions", &d, 23, row_json);
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn function_nodes_draw_per_row() {
    let d = functions();
    let rows = rows_match_alone("functions", &d, 5);
    assert_eq!(distinct(rows.iter().map(|r| r["r"].clone())), ROWS);
    assert_eq!(distinct(rows.iter().map(|r| r["v1"].clone())), ROWS);
    for r in &rows {
        assert_ne!(r["r"], r["q"]);
    }
    columns_match_alone("functions", &d, 5, row_json);
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn table_outputs_draw_per_row() {
    let d = tables();
    rows_match_alone("tables", &d, 11);
    columns_match_alone("tables", &d, 11, row_json);
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn switch_branches_draw_per_row() {
    let d = branches();
    rows_match_alone("branches", &d, 13);
    columns_match_alone("branches", &d, 13, row_json);
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn rows_with_different_seeds_differ() {
    let d = top_level();
    let a = block(d.evaluate_with_opts(row(1), options(TraceFormat::Off, 1))).unwrap();
    let b = block(d.evaluate_with_opts(row(1), options(TraceFormat::Off, 2))).unwrap();
    assert_ne!(
        serde_json::to_value(a.result).unwrap(),
        serde_json::to_value(b.result).unwrap()
    );
}

#[test]
fn a_seed_is_written_only_when_a_row_draws() {
    let d = decision(
        chain(vec![
            input(),
            expressions("calc", &[("r", "x > 3 ? rand(100) : 0")], json!({})),
            output(),
        ]),
        &[],
    );
    let inputs: Vec<Variable> = (0..ROWS).map(row).collect();
    let batch = block(d.evaluate_batch(&inputs, options(TraceFormat::V1, 9)));
    for (x, result) in batch.into_iter().enumerate() {
        let t = serde_json::to_value(result.unwrap().trace.unwrap()).unwrap();
        match x > 3 {
            true => assert_eq!(t["seed"], json!(row_seed(9, x))),
            false => assert!(t.get("seed").is_none(), "row {x}: {t}"),
        }
    }
}

fn policy_document() -> zen_engine::policy::PolicyDocument {
    serde_json::from_value(json!({
        "blocks": [
            {"id": "dm-customer", "type": "dataModel", "props": {"data": {
                "name": "customer",
                "properties": [
                    {"id": "p0", "name": "age", "type": "number", "array": false, "optional": false},
                    {"id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false}
                ]
            }}},
            {"id": "dm-company", "type": "dataModel", "props": {"data": {
                "name": "company",
                "properties": [
                    {"id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false}
                ]
            }}},
            {"id": "e1", "type": "expression", "props": {"data": {
                "key": "customer.lucky", "value": "rand(1000000) + customer.age"
            }}},
            {"id": "e2", "type": "expression", "props": {"data": {
                "key": "company.score", "value": "rand(1000) + company.revenue"
            }}},
            {"id": "e3", "type": "expression", "props": {"data": {
                "key": "customer.draws", "value": "map(customer.companies as c, rand(1000))"
            }}},
            {"id": "e4", "type": "expression", "props": {"data": {
                "key": "customer.after", "value": "rand(1000000)"
            }}}
        ]
    }))
    .unwrap()
}

fn policy_input(x: usize) -> Value {
    let companies: Vec<Value> = (0..(1 + x * 11 % 40))
        .map(|i| json!({"revenue": i * 10}))
        .collect();
    json!({"customer": {"age": x, "companies": companies}})
}

fn policy_request(x: usize, trace: TraceFormat) -> zen_engine::policy::EvaluateRequest {
    zen_engine::policy::EvaluateRequest {
        policy_path: Arc::from("policy"),
        input: Variable::from(policy_input(x)),
        goals: Vec::new(),
        trace,
    }
}

fn pinned<R>(seed: u64, f: impl FnOnce() -> R) -> R {
    zen_engine::clock::Clock::new(parse_instant(NOW), parse_timezone("UTC"), Some(seed)).scope(f)
}

fn policy_trace(result: &zen_engine::policy::EvaluationResult) -> Value {
    untimed(serde_json::to_value(result.trace.as_ref().unwrap()).unwrap())
}

#[test]
fn policy_rows_draw_from_their_streams() {
    compiled();
    let mut ws = zen_engine::Workspace::new();
    ws.set_policy("policy", policy_document());
    let base = 29;
    let singles: Vec<(Value, Value)> = (0..ROWS)
        .map(|x| {
            let seed = row_seed(base, x);
            let traced = pinned(seed, || ws.evaluate(&policy_request(x, TraceFormat::V1))).unwrap();
            let plain = pinned(seed, || ws.evaluate(&policy_request(x, TraceFormat::Off))).unwrap();
            let value = serde_json::to_value(&traced.output).unwrap();
            assert_eq!(
                value,
                serde_json::to_value(&plain.output).unwrap(),
                "row {x}"
            );
            (value, policy_trace(&traced))
        })
        .collect();
    for trace in [TraceFormat::V1, TraceFormat::Off] {
        let requests: Vec<_> = (0..ROWS).map(|x| policy_request(x, trace)).collect();
        let batch = pinned(base, || ws.evaluate_batch(&requests));
        for (x, result) in batch.into_iter().enumerate() {
            let result = result.unwrap();
            assert_eq!(
                serde_json::to_value(&result.output).unwrap(),
                singles[x].0,
                "policy batch row {x} ({trace:?})"
            );
            if trace == TraceFormat::V1 {
                assert_eq!(policy_trace(&result), singles[x].1, "policy trace {x}");
            }
        }
    }
    let rows: Vec<Value> = (0..ROWS).map(policy_input).collect();
    let built = Built::nested(&rows).expect("columns");
    let columns = built.columns();
    let output = pinned(base, || {
        ws.evaluate_columns(&Arc::from("policy"), &[], &columns)
    })
    .unwrap();
    for (x, (single, _)) in singles.iter().enumerate() {
        assert_eq!(
            &serde_json::to_value(output.row(x)).unwrap(),
            single,
            "policy columnar row {x}"
        );
    }
    let distinct_lucky = distinct(singles.iter().map(|(v, _)| v["customer"]["lucky"].clone()));
    assert_eq!(distinct_lucky, ROWS);
    assert_eq!(unrowed_draws(), 0);
}

#[test]
fn engine_policy_batches_draw_per_row() {
    compiled();
    let loader = Arc::new(MemoryLoader::default());
    loader.add(
        "policy",
        DecisionContent::Policy(zen_engine::model::PolicyContent(
            Arc::new(policy_document()),
        )),
    );
    let engine = DecisionEngine::default().with_loader(loader);
    let inputs: Vec<Variable> = (0..ROWS).map(|x| Variable::from(policy_input(x))).collect();
    for trace in [TraceFormat::V1, TraceFormat::Off] {
        let batch = block(engine.evaluate_batch("policy", &inputs, options(trace, 31)));
        for (x, result) in batch.into_iter().enumerate() {
            let result = result.unwrap();
            let single = block(engine.evaluate_with_opts(
                "policy",
                inputs[x].clone(),
                options(trace, row_seed(31, x)),
            ))
            .unwrap();
            assert_eq!(
                serde_json::to_value(&result.result).unwrap(),
                serde_json::to_value(&single.result).unwrap(),
                "engine policy row {x} ({trace:?})"
            );
            if let (Some(a), Some(b)) = (result.trace, single.trace) {
                let (a, b) = (
                    serde_json::to_value(a).unwrap(),
                    serde_json::to_value(b).unwrap(),
                );
                assert_eq!(a["seed"], json!(row_seed(31, x)));
                assert_eq!(untimed(a), untimed(b), "engine policy trace {x}");
            }
        }
    }
    assert_eq!(unrowed_draws(), 0);
}

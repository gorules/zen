use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::clock::{parse_instant, parse_timezone, row_seed};
use zen_engine::columns::{Column, Columns, Values};
use zen_engine::model::DecisionContent;
use zen_engine::trace::TraceFormat;
use zen_engine::{Decision, DecisionEngine, EvaluationOptions, Variable};

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

fn graph(middle: Value) -> Decision {
    zen_engine::ZEN_CONFIG
        .compiled
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let content: DecisionContent = serde_json::from_value(json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            middle,
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "calc", "type": "edge"},
            {"id": "e2", "sourceId": "calc", "targetId": "out", "type": "edge"}
        ]
    }))
    .unwrap();
    DecisionEngine::default()
        .create_decision(Arc::new(content))
        .unwrap()
}

fn expressions() -> Decision {
    graph(json!({
        "id": "calc", "name": "calc", "type": "expressionNode",
        "content": {"expressions": [
            {"id": "x0", "key": "v", "value": "1"},
            {"id": "x1", "key": "v", "value": "x + 1"},
            {"id": "x2", "key": "r", "value": "rand(1000000)"},
            {"id": "x3", "key": "s", "value": "rand(1000000)"},
            {"id": "x4", "key": "t", "value": "d().timestamp()"},
            {"id": "x5", "key": "today", "value": "d().isToday()"},
            {"id": "x6", "key": "zone", "value": "d().offsetName()"},
            {"id": "x7", "key": "parsed", "value": "d('2026-01-02 10:00').timestamp()"}
        ]}
    }))
}

fn functions() -> Decision {
    graph(json!({
        "id": "calc", "name": "calc", "type": "functionNode",
        "content": {"source": "export const handler = async (input) => ({ now: Date.now(), iso: new Date().toISOString(), same: Date.now() === new Date().getTime(), r: Math.random(), q: Math.random(), x: input.x })"}
    }))
}

fn input(x: i64) -> Variable {
    Variable::from(json!({"x": x}))
}

fn options(trace: TraceFormat, now: &str, seed: u64) -> EvaluationOptions {
    EvaluationOptions {
        trace,
        now: parse_instant(now),
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

fn evaluated(d: &Decision, x: i64, o: EvaluationOptions) -> (Value, Option<Value>) {
    let r = block(d.evaluate_with_opts(input(x), o)).unwrap();
    let doc = r.trace.map(|t| serde_json::to_value(&t).unwrap());
    (serde_json::to_value(&r.result).unwrap(), doc)
}

const NOW: &str = "2026-11-04T09:30:00.125Z";

#[test]
fn supplied_now_seed_and_zone_reproduce_results_and_traces() {
    {
        let d = expressions();
        let (a, da) = evaluated(&d, 4, options(TraceFormat::V1, NOW, 99));
        let (b, db) = evaluated(&d, 4, options(TraceFormat::V1, NOW, 99));
        assert_eq!(a, b);
        assert_eq!(untimed(da.clone().unwrap()), untimed(db.unwrap()));
        let doc = da.unwrap();
        assert_eq!(doc["evaluatedAt"], json!(NOW));
        assert_eq!(doc["timezone"], json!("Europe/Berlin"));
        assert_eq!(doc["seed"], json!(99));
        assert_eq!(a["t"], json!(1_793_784_600_125i64));
        assert_eq!(a["today"], json!(true));
        assert_eq!(a["zone"], json!("Europe/Berlin"));
        assert_eq!(a["parsed"], json!(1_767_344_400_000i64));
        assert_ne!(a["r"], a["s"]);
        let (c, _) = evaluated(&d, 4, options(TraceFormat::Off, NOW, 99));
        assert_eq!(a, c);
        let (other, _) = evaluated(&d, 4, options(TraceFormat::Off, NOW, 100));
        assert_ne!((&a["r"], &a["s"]), (&other["r"], &other["s"]));
    }
}

fn real_clock(millis: &Value) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    millis
        .as_i64()
        .is_some_and(|m| (now - m).abs() < 60_000 && m != 1_793_784_600_125i64)
}

#[test]
fn the_interpreted_engine_ignores_pinning() {
    let v2 = functions();
    let v1 = graph(json!({
        "id": "calc", "name": "calc", "type": "functionNode",
        "content": "const handler = (input) => ({ now: Date.now(), global: now, r: Math.random(), q: Math.random() })"
    }));
    let d = expressions();
    zen_engine::ZEN_CONFIG
        .compiled
        .store(false, std::sync::atomic::Ordering::Relaxed);
    for format in [TraceFormat::V1, TraceFormat::Legacy] {
        let (a, doc) = evaluated(&d, 4, options(format, NOW, 99));
        let (b, _) = evaluated(&d, 4, options(format, NOW, 99));
        assert!(real_clock(&a["t"]), "{a}");
        assert_ne!((&a["r"], &a["s"]), (&b["r"], &b["s"]));
        let doc = doc.unwrap();
        assert!(doc.get("version").is_none(), "{doc}");
        assert!(doc.get("evaluatedAt").is_none());
        assert_eq!(doc["calc"]["traceData"]["t"]["result"], a["t"]);
        for f in [&v2, &v1] {
            let (a, doc) = evaluated(f, 1, options(format, NOW, 6));
            let (b, _) = evaluated(f, 1, options(format, NOW, 6));
            assert!(real_clock(&a["now"]), "{a}");
            assert_ne!((&a["r"], &a["q"]), (&b["r"], &b["q"]));
            assert!(doc.unwrap().get("version").is_none());
        }
    }
    let engine = policy_engine();
    let run = |o: EvaluationOptions| {
        let r = block(engine.evaluate_with_opts(
            "policy",
            Variable::from(json!({"customer": {"age": 3}})),
            o,
        ))
        .unwrap();
        (
            serde_json::to_value(&r.result).unwrap(),
            r.trace.map(|t| serde_json::to_value(&t).unwrap()),
        )
    };
    let (a, doc) = run(options(TraceFormat::V1, NOW, 12));
    let (b, _) = run(options(TraceFormat::V1, NOW, 12));
    assert!(real_clock(&a["customer"]["at"]), "{a}");
    assert_ne!(a["customer"]["lucky"], b["customer"]["lucky"]);
    let doc = doc.unwrap();
    assert!(doc.get("version").is_none(), "{doc}");
    assert!(doc["properties"].is_object());
    assert!(doc["executions"].is_array());
    zen_engine::ZEN_CONFIG
        .compiled
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn an_unpinned_evaluation_replays_from_its_trace() {
    for d in [expressions(), functions()] {
        let (a, doc) = evaluated(
            &d,
            7,
            EvaluationOptions {
                trace: TraceFormat::V1,
                ..Default::default()
            },
        );
        let doc = doc.unwrap();
        let at = doc["evaluatedAt"].as_str().unwrap();
        let zone = doc["timezone"].as_str().unwrap();
        let seed = doc["seed"]
            .as_u64()
            .expect("a drawing evaluation writes its seed");
        let replay = EvaluationOptions {
            trace: TraceFormat::V1,
            now: parse_instant(at),
            timezone: parse_timezone(zone),
            seed: Some(seed),
            ..Default::default()
        };
        let (b, again) = evaluated(&d, 7, replay);
        assert_eq!(a, b);
        assert_eq!(untimed(doc), untimed(again.unwrap()));
    }
}

#[test]
fn function_nodes_see_the_pinned_clock_and_stream() {
    let d = functions();
    let (a, doc) = evaluated(&d, 1, options(TraceFormat::V1, NOW, 5));
    assert_eq!(a["now"], json!(1_793_784_600_125i64));
    assert_eq!(a["iso"], json!(NOW));
    assert_eq!(a["same"], json!(true));
    assert_ne!(a["r"], a["q"]);
    let (b, _) = evaluated(&d, 1, options(TraceFormat::Off, NOW, 5));
    assert_eq!(a, b);
    assert_eq!(doc.unwrap()["seed"], json!(5));
}

#[test]
fn a_plain_evaluation_writes_no_seed() {
    let d = graph(json!({
        "id": "calc", "name": "calc", "type": "expressionNode",
        "content": {"expressions": [{"id": "a", "key": "a", "value": "x * 2"}]}
    }));
    let (_, doc) = evaluated(
        &d,
        1,
        EvaluationOptions {
            trace: TraceFormat::V1,
            ..Default::default()
        },
    );
    let doc = doc.unwrap();
    assert!(doc["evaluatedAt"].is_string());
    assert!(doc["timezone"].is_string());
    assert!(doc.get("seed").is_none());
}

#[test]
fn repeated_keys_are_stored_as_rows() {
    let d = expressions();
    let (_, doc) = evaluated(&d, 4, options(TraceFormat::V1, NOW, 1));
    let doc = doc.unwrap();
    assert!(doc["requires"]
        .as_array()
        .unwrap()
        .contains(&json!("result-rows")));
    let calc = doc["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "calc")
        .unwrap();
    let rows = calc["results"].as_array().unwrap();
    assert_eq!(rows[0], json!({"id": "x0", "key": "v", "value": 1}));
    assert_eq!(rows[1], json!({"id": "x1", "key": "v", "value": 5}));
    assert_eq!(rows.len(), 8);
    let r = block(d.evaluate_with_opts(input(4), options(TraceFormat::Legacy, NOW, 1))).unwrap();
    let legacy = serde_json::to_value(r.trace.unwrap()).unwrap();
    assert_eq!(legacy["calc"]["traceData"]["v"], json!({"result": 5}));
}

#[test]
fn batch_rows_replay_alone() {
    let d = expressions();
    let inputs: Vec<Variable> = (0..6).map(input).collect();
    for trace in [TraceFormat::V1, TraceFormat::Off] {
        let batch = block(d.evaluate_batch(&inputs, options(trace, NOW, 31)));
        let mut seen = Vec::new();
        for (row, result) in batch.into_iter().enumerate() {
            let result = result.unwrap();
            let got = serde_json::to_value(&result.result).unwrap();
            let seed = row_seed(31, row);
            let (alone, doc) = evaluated(&d, row as i64, options(TraceFormat::V1, NOW, seed));
            assert_eq!(got, alone, "row {row}");
            if let Some(t) = result.trace {
                let t = serde_json::to_value(&t).unwrap();
                assert_eq!(t["seed"], json!(seed));
                assert_eq!(t["evaluatedAt"], json!(NOW));
                assert_eq!(untimed(t), untimed(doc.unwrap()));
            }
            seen.push(got["r"].clone());
        }
        seen.dedup();
        assert!(seen.len() > 1, "rows draw from their own streams");
    }
}

#[test]
fn columnar_rows_replay_alone() {
    let d = expressions();
    let xs: Vec<rust_decimal::Decimal> = (0..6).map(rust_decimal::Decimal::from).collect();
    let bits = vec![u64::MAX];
    let columns = Columns::new(6).column("x", Column::with_validity(Values::Dec(&xs), &bits, 0));
    for trace in [TraceFormat::V1, TraceFormat::Off] {
        let output = block(d.evaluate_columns(&columns, options(trace, NOW, 8)));
        for row in 0..6 {
            let got = serde_json::to_value(output.row(row)).unwrap();
            let seed = row_seed(8, row);
            let (alone, doc) = evaluated(&d, row as i64, options(TraceFormat::V1, NOW, seed));
            assert_eq!(got, alone, "row {row}");
            if trace == TraceFormat::V1 {
                let t: Value = serde_json::from_str(&output.trace_json(row).unwrap()).unwrap();
                assert_eq!(untimed(t), untimed(doc.unwrap()));
            }
        }
    }
}

#[test]
fn batches_without_draws_share_one_clock() {
    let d = graph(json!({
        "id": "calc", "name": "calc", "type": "expressionNode",
        "content": {"expressions": [
            {"id": "a", "key": "a", "value": "x * 2"},
            {"id": "b", "key": "t", "value": "d().timestamp()"}
        ]}
    }));
    let inputs: Vec<Variable> = (0..40).map(input).collect();
    let batch = block(d.evaluate_batch(
        &inputs,
        EvaluationOptions {
            trace: TraceFormat::V1,
            ..Default::default()
        },
    ));
    let docs: Vec<Value> = batch
        .into_iter()
        .map(|r| serde_json::to_value(r.unwrap().trace.unwrap()).unwrap())
        .collect();
    let at = &docs[0]["evaluatedAt"];
    assert!(at.is_string());
    assert!(docs
        .iter()
        .all(|d| &d["evaluatedAt"] == at && d.get("seed").is_none()));
    let replay = options(TraceFormat::V1, at.as_str().unwrap(), 0);
    let (first, doc) = evaluated(
        &d,
        0,
        EvaluationOptions {
            seed: None,
            timezone: parse_timezone(docs[0]["timezone"].as_str().unwrap()),
            ..replay
        },
    );
    assert_eq!(untimed(docs[0].clone()), untimed(doc.unwrap()));
    assert_eq!(first["a"], json!(0));
}

#[test]
fn repeated_keys_are_rows_in_every_compiled_path() {
    let single = graph(json!({
        "id": "calc", "name": "calc", "type": "expressionNode",
        "content": {"expressions": [
            {"id": "a0", "key": "a", "value": "x * 2"},
            {"id": "a1", "key": "a", "value": "$.a + 1"},
            {"id": "b0", "key": "b", "value": "x"}
        ]}
    }));
    let looped = graph(json!({
        "id": "calc", "name": "calc", "type": "expressionNode",
        "content": {
            "inputField": "items", "executionMode": "loop",
            "expressions": [
                {"id": "a0", "key": "a", "value": "n * 2"},
                {"id": "a1", "key": "a", "value": "$.a + 1"}
            ]
        }
    }));
    let at = options(TraceFormat::V1, NOW, 3);
    for (d, looping) in [(single, false), (looped, true)] {
        let inputs: Vec<Variable> = (0..20)
            .map(|x| Variable::from(json!({"x": x, "items": [{"n": x}, {"n": x + 1}]})))
            .collect();
        let batch = block(d.evaluate_batch(&inputs, at));
        let xs: Vec<rust_decimal::Decimal> = (0..20).map(rust_decimal::Decimal::from).collect();
        let bits = vec![u64::MAX];
        let columns =
            Columns::new(20).column("x", Column::with_validity(Values::Dec(&xs), &bits, 0));
        let columnar = (!looping).then(|| block(d.evaluate_columns(&columns, at)));
        for (row, result) in batch.into_iter().enumerate() {
            let alone = block(d.evaluate_with_opts(inputs[row].clone(), at)).unwrap();
            let alone = serde_json::to_value(alone.trace.unwrap()).unwrap();
            let calc = alone["nodes"][1]["results"].clone();
            let rows = match looping {
                false => calc.clone(),
                true => calc[0].clone(),
            };
            assert_eq!(rows[0]["id"], json!("a0"), "{alone}");
            assert_eq!(
                rows[1]["value"],
                json!(rows[0]["value"].as_i64().unwrap() + 1)
            );
            let batched = serde_json::to_value(result.unwrap().trace.unwrap()).unwrap();
            assert_eq!(untimed(batched), untimed(alone.clone()), "batch row {row}");
            if let Some(output) = &columnar {
                let t: Value = serde_json::from_str(&output.trace_json(row).unwrap()).unwrap();
                assert_eq!(untimed(t), untimed(alone), "columnar row {row}");
            }
        }
    }
}

fn policy_engine() -> DecisionEngine {
    let policy: zen_engine::policy::PolicyDocument = serde_json::from_value(json!({
        "blocks": [
            {"id": "dm", "type": "dataModel", "props": {"data": {"name": "customer", "properties": [
                {"id": "p1", "name": "age", "type": "number", "array": false, "optional": false}
            ]}}, "children": []},
            {"id": "e1", "type": "expression", "props": {"data": {
                "key": "customer.lucky", "value": "rand(1000000) + customer.age"
            }}},
            {"id": "e2", "type": "expression", "props": {"data": {
                "key": "customer.at", "value": "d().timestamp()"
            }}}
        ]
    }))
    .unwrap();
    let loader = Arc::new(zen_engine::loader::MemoryLoader::default());
    loader.add(
        "policy",
        DecisionContent::Policy(zen_engine::model::PolicyContent(Arc::new(policy))),
    );
    DecisionEngine::default().with_loader(loader)
}

#[test]
fn policies_pin_the_clock_and_stream() {
    let engine = policy_engine().compiled_engine();
    let run = |o: EvaluationOptions| {
        let r = block(engine.evaluate_with_opts(
            "policy",
            Variable::from(json!({"customer": {"age": 3}})),
            o,
        ))
        .unwrap();
        (
            serde_json::to_value(&r.result).unwrap(),
            r.trace.map(|t| serde_json::to_value(&t).unwrap()),
        )
    };
    let (a, doc) = run(options(TraceFormat::V1, NOW, 12));
    let (b, _) = run(options(TraceFormat::Off, NOW, 12));
    assert_eq!(a, b);
    assert_eq!(a["customer"]["at"], json!(1_793_784_600_125i64));
    let doc = doc.unwrap();
    assert_eq!(doc["kind"], json!("policy"));
    assert_eq!(doc["evaluatedAt"], json!(NOW));
    assert_eq!(doc["seed"], json!(12));
    let inputs: Vec<Variable> = (0..4)
        .map(|age| Variable::from(json!({"customer": {"age": age}})))
        .collect();
    let batch = block(engine.evaluate_batch("policy", &inputs, options(TraceFormat::Off, NOW, 12)));
    for (row, result) in batch.into_iter().enumerate() {
        let got = serde_json::to_value(result.unwrap().result).unwrap();
        let r = block(engine.evaluate_with_opts(
            "policy",
            inputs[row].clone(),
            options(TraceFormat::Off, NOW, row_seed(12, row)),
        ))
        .unwrap();
        assert_eq!(got, serde_json::to_value(r.result).unwrap(), "row {row}");
    }
}

#[test]
fn v1_function_nodes_see_the_pinned_clock_and_stream() {
    {
        let d = graph(json!({
            "id": "calc", "name": "calc", "type": "functionNode",
            "content": "const handler = (input) => ({ now: Date.now(), global: now, iso: new Date().toISOString(), r: Math.random(), q: Math.random() })"
        }));
        let (a, doc) = evaluated(&d, 1, options(TraceFormat::V1, NOW, 6));
        assert_eq!(a["now"], json!(1_793_784_600_125i64));
        assert_eq!(a["global"], json!(1_793_784_600_125i64));
        assert_eq!(a["iso"], json!(NOW));
        assert_ne!(a["r"], a["q"]);
        let (b, _) = evaluated(&d, 1, options(TraceFormat::Off, NOW, 6));
        assert_eq!(a, b);
        assert_eq!(doc.unwrap()["seed"], json!(6));
    }
}

use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use zen_engine::context::{parse_instant, parse_locale, parse_timezone};
use zen_engine::loader::{DecisionLoader, LoaderResponse, MemoryLoader};
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationOptions, Variable};

#[derive(Debug, Default)]
struct Yielding {
    memory: MemoryLoader,
    inflight: AtomicUsize,
    overlapped: AtomicUsize,
}

impl DecisionLoader for Yielding {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        Box::pin(async move {
            let others = self.inflight.fetch_add(1, Ordering::SeqCst);
            self.overlapped.fetch_max(others, Ordering::SeqCst);
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            self.memory.load(key).await
        })
    }
}

fn child() -> Value {
    json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            {"id": "calc", "name": "calc", "type": "expressionNode", "content": {"expressions": [
                {"id": "c1", "key": "c", "value": "rand(1000000)"},
                {"id": "c2", "key": "ct", "value": "d().timestamp()"},
                {"id": "c3", "key": "cz", "value": "d().offsetName()"}
            ]}},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "calc", "type": "edge"},
            {"id": "e2", "sourceId": "calc", "targetId": "out", "type": "edge"}
        ]
    })
}

fn main_graph() -> Value {
    json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            {"id": "calc", "name": "calc", "type": "expressionNode", "content": {"expressions": [
                {"id": "x1", "key": "r", "value": "rand(1000000)"},
                {"id": "x2", "key": "t", "value": "d().timestamp()"},
                {"id": "x3", "key": "zone", "value": "d().offsetName()"},
                {"id": "x4", "key": "local", "value": "d('2026-01-02 10:00').timestamp()"}
            ]}},
            {"id": "sub", "name": "sub", "type": "decisionNode", "content": {
                "key": "child", "transformAttributes": {"executionMode": "single", "passThrough": true}
            }},
            {"id": "fn", "name": "fn", "type": "functionNode", "content": {
                "source": "import zen from 'zen';\nexport const handler = async (input) => {\n  const before = Math.random();\n  const nested = await zen.evaluate('child', input);\n  const local = new Date().toString();\n  const formatted = new Date().toLocaleString();\n  const intl = Intl.DateTimeFormat().resolvedOptions();\n  return { ...input, before, nested: nested.result, now: Date.now(), after: Math.random(), local, formatted, jsZone: intl.timeZone, jsLocale: intl.locale };\n};"
            }},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "calc", "type": "edge"},
            {"id": "e2", "sourceId": "calc", "targetId": "sub", "type": "edge"},
            {"id": "e3", "sourceId": "sub", "targetId": "fn", "type": "edge"},
            {"id": "e4", "sourceId": "fn", "targetId": "out", "type": "edge"},
            {"id": "e5", "sourceId": "calc", "targetId": "fn", "type": "edge"}
        ]
    })
}

fn engine() -> (DecisionEngine, Arc<Yielding>) {
    zen_engine::ZEN_CONFIG
        .compiled
        .store(true, Ordering::Relaxed);
    let loader = Arc::new(Yielding::default());
    for (key, graph) in [("main", main_graph()), ("child", child())] {
        loader.memory.add(
            key,
            serde_json::from_value::<DecisionContent>(graph).unwrap(),
        );
    }
    (
        DecisionEngine::default().with_loader(loader.clone()),
        loader,
    )
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[derive(Clone, Copy)]
struct Pin3 {
    now: &'static str,
    zone: &'static str,
    seed: u64,
    locale: &'static str,
    local: &'static str,
    formatted: &'static str,
}

const A: Pin3 = Pin3 {
    now: "2026-11-04T09:30:00.125Z",
    zone: "Europe/Berlin",
    seed: 11,
    locale: "de-DE",
    local: "Wed Nov 04 2026 10:30:00 GMT+0100",
    formatted: "4.11.2026, 10:30:00",
};

const B: Pin3 = Pin3 {
    now: "2031-02-17T23:15:42.500Z",
    zone: "America/New_York",
    seed: 97,
    locale: "en-US",
    local: "Mon Feb 17 2031 18:15:42 GMT-0500",
    formatted: "02/17/2031, 06:15:42 PM",
};

fn options(pin: Pin3, trace: bool) -> EvaluationOptions {
    EvaluationOptions {
        trace,
        now: parse_instant(pin.now),
        timezone: parse_timezone(pin.zone),
        seed: Some(pin.seed),
        locale: parse_locale(pin.locale),
        ..Default::default()
    }
}

fn millis(pin: Pin3) -> i64 {
    parse_instant(pin.now).unwrap().timestamp_millis()
}

fn input() -> Variable {
    Variable::from(json!({"x": 1}))
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

async fn single(engine: &DecisionEngine, pin: Pin3, trace: bool) -> (Value, Option<Value>) {
    let response = engine
        .evaluate_with_opts("main", input(), options(pin, trace))
        .await
        .unwrap();
    (
        serde_json::to_value(&response.result).unwrap(),
        response
            .trace
            .map(|t| untimed(serde_json::to_value(&t).unwrap())),
    )
}

async fn batch(engine: &DecisionEngine, pin: Pin3) -> Vec<Value> {
    let inputs: Vec<Variable> = (0..4).map(|x| Variable::from(json!({"x": x}))).collect();
    engine
        .evaluate_batch("main", &inputs, options(pin, false))
        .await
        .into_iter()
        .map(|r| serde_json::to_value(r.unwrap().result).unwrap())
        .collect()
}

fn sees_its_own_clock(value: &Value, pin: Pin3) {
    let at = millis(pin);
    assert_eq!(value["t"], json!(at), "{value}");
    assert_eq!(value["now"], json!(at), "{value}");
    assert_eq!(value["ct"], json!(at), "{value}");
    assert_eq!(value["nested"]["ct"], json!(at), "{value}");
    assert_eq!(value["zone"], json!(pin.zone), "{value}");
    assert_eq!(value["cz"], json!(pin.zone), "{value}");
    assert_eq!(value["nested"]["cz"], json!(pin.zone), "{value}");
    assert_eq!(value["jsZone"], json!(pin.zone), "{value}");
    assert_eq!(value["jsLocale"], json!(pin.locale), "{value}");
    assert_eq!(value["local"], json!(pin.local), "{value}");
    assert_eq!(value["formatted"], json!(pin.formatted), "{value}");
}

struct Expected {
    a: (Value, Option<Value>),
    b: (Value, Option<Value>),
    batch_a: Vec<Value>,
    batch_b: Vec<Value>,
}

fn expected(engine: &DecisionEngine) -> Expected {
    let rt = runtime();
    let expected = Expected {
        a: rt.block_on(single(engine, A, true)),
        b: rt.block_on(single(engine, B, true)),
        batch_a: rt.block_on(batch(engine, A)),
        batch_b: rt.block_on(batch(engine, B)),
    };
    sees_its_own_clock(&expected.a.0, A);
    sees_its_own_clock(&expected.b.0, B);
    assert_ne!(expected.a.0["r"], expected.b.0["r"]);
    assert_ne!(expected.a.0["local"], expected.b.0["local"]);
    expected
}

#[test]
fn interleaved_evaluations_on_one_thread_see_their_own_clock() {
    let (engine, loader) = engine();
    let want = expected(&engine);
    assert_eq!(loader.overlapped.load(Ordering::SeqCst), 0);
    let rt = runtime();
    for _ in 0..3 {
        let (a, b, batch_b, batch_a) = rt.block_on(async {
            tokio::join!(
                single(&engine, A, true),
                single(&engine, B, true),
                batch(&engine, B),
                batch(&engine, A),
            )
        });
        assert_eq!(a, want.a);
        assert_eq!(b, want.b);
        assert_eq!(batch_a, want.batch_a);
        assert_eq!(batch_b, want.batch_b);
    }
    assert!(loader.overlapped.load(Ordering::SeqCst) > 0);
    for (value, pin) in want
        .batch_a
        .iter()
        .map(|v| (v, A))
        .chain(want.batch_b.iter().map(|v| (v, B)))
    {
        sees_its_own_clock(value, pin);
    }
}

#[test]
fn evaluations_on_two_threads_see_their_own_clock() {
    let (engine, _) = engine();
    let want = expected(&engine);
    std::thread::scope(|scope| {
        let workers: Vec<_> = [(A, &want.a, &want.batch_a), (B, &want.b, &want.batch_b)]
            .into_iter()
            .map(|(pin, alone, batched)| {
                let engine = &engine;
                scope.spawn(move || {
                    let rt = runtime();
                    for _ in 0..20 {
                        assert_eq!(&rt.block_on(single(engine, pin, true)), alone);
                        assert_eq!(&rt.block_on(batch(engine, pin)), batched);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
}

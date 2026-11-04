use serde::Deserialize;
use serde_json::{json, Value};
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use zen_engine::columns::{Column, Columns, Values};
use zen_engine::loader::{FilesystemLoader, FilesystemLoaderOptions, MemoryLoader};
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EvaluationOptions, ZEN_CONFIG};
use zen_expression::Variable;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "mimalloc")]
static INNER: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(not(feature = "mimalloc"))]
static INNER: System = System;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = INNER.alloc(layout);
        CALLS.fetch_add(1, Ordering::Relaxed);
        if !p.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        INNER.dealloc(ptr, layout);
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = INNER.realloc(ptr, layout, new_size);
        if !p.is_null() {
            if new_size >= layout.size() {
                let now = CURRENT.fetch_add(new_size - layout.size(), Ordering::Relaxed) + new_size
                    - layout.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        p
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Off,
    Legacy,
    #[cfg(not(feature = "base"))]
    V1,
}

impl Mode {
    fn all() -> Vec<Mode> {
        #[cfg(feature = "base")]
        return vec![Mode::Off, Mode::Legacy];
        #[cfg(not(feature = "base"))]
        return vec![Mode::Off, Mode::V1, Mode::Legacy];
    }

    fn name(self) -> &'static str {
        match self {
            Mode::Off => "off",
            #[cfg(feature = "base")]
            Mode::Legacy => "legacy (base)",
            #[cfg(not(feature = "base"))]
            Mode::Legacy => "legacy via restore",
            #[cfg(not(feature = "base"))]
            Mode::V1 => "v1",
        }
    }

    #[cfg(feature = "base")]
    fn options(self) -> EvaluationOptions {
        EvaluationOptions {
            trace: self != Mode::Off,
            max_depth: 10,
            ..Default::default()
        }
    }

    #[cfg(not(feature = "base"))]
    fn options(self) -> EvaluationOptions {
        use zen_engine::trace::TraceFormat;
        EvaluationOptions {
            trace: match self {
                Mode::Off => TraceFormat::Off,
                Mode::Legacy => TraceFormat::Legacy,
                Mode::V1 => TraceFormat::V1,
            },
            max_depth: 10,
            ..Default::default()
        }
    }
}

const CATEGORIES: [&str; 6] = [
    "electronics",
    "grocery",
    "apparel",
    "home",
    "toys",
    "garden",
];
const SEGMENTS: [&str; 4] = ["gold", "silver", "bronze", "new"];
const COUNTRIES: [&str; 5] = ["GB", "US", "DE", "NG", "BR"];

fn payload(seed: usize, items: usize) -> Value {
    let rows: Vec<Value> = (0..items)
        .map(|k| {
            let h = (seed * 7919 + k * 104_729) % 1_000_003;
            json!({
                "sku": format!("sku-{:06}", h % 1_000_000),
                "title": format!("Product {h} in a reasonably descriptive catalogue title, size {}", h % 13),
                "category": CATEGORIES[h % 6],
                "qty": h % 5 + 1,
                "price": (h % 90_000) as f64 / 100.0 + 0.99,
                "warehouse": format!("wh-{}", h % 17),
                "tags": ["catalog", if h.is_multiple_of(3) { "promo" } else { "regular" }, "season-26"],
                "attrs": {"color": (["red", "blue", "black", "white"][h % 4]), "weightKg": (h % 300) as f64 / 10.0, "fragile": h.is_multiple_of(7)}
            })
        })
        .collect();
    let total: f64 = rows
        .iter()
        .map(|r| r["price"].as_f64().unwrap() * r["qty"].as_f64().unwrap())
        .sum();
    json!({
        "customer": {"id": format!("cus_{}", seed * 31 % 99_991), "segment": SEGMENTS[seed % 4], "country": COUNTRIES[seed % 5],
            "history": {"orders": seed * 13 % 400, "returns": seed % 9, "lifetimeValue": (seed * 977 % 50_000) as f64 + 0.5}},
        "order": {"id": format!("ord_{seed:08}"), "channel": (["web", "app", "store"][seed % 3]), "currency": "EUR", "total": (total * 100.0).round() / 100.0, "items": rows},
        "device": {"ip": format!("203.0.113.{}", seed % 250), "fingerprint": format!("{:016x}", seed * 0x9e37_79b9_7f4a_7c15usize)}
    })
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({"id": id, "name": id, "type": kind, "content": content})
}

fn table(i: usize, loop_items: bool) -> Value {
    if loop_items {
        return json!({
            "hitPolicy": "first", "passThrough": false, "inputField": "order.items", "outputPath": format!("lines.n{i}"), "executionMode": "loop",
            "inputs": [{"id": "i0", "name": "category", "field": "category"}, {"id": "i1", "name": "price", "field": "price"}, {"id": "i2", "name": "qty", "field": "qty"}],
            "outputs": [{"id": "o0", "name": "band", "field": "band"}],
            "rules": [
                {"_id": "r0", "i0": "'electronics'", "i1": "> 500", "i2": "", "o0": "'review'"},
                {"_id": "r1", "i0": "", "i1": "> 800", "i2": "> 2", "o0": "'hold'"},
                {"_id": "r2", "i0": "'toys', 'garden'", "i1": "", "i2": "", "o0": "'light'"},
                {"_id": "r3", "i0": "", "i1": "", "i2": "", "o0": "'ok'"}
            ]
        });
    }
    let big_input = i.is_multiple_of(5);
    let mut inputs = vec![
        json!({"id": "i0", "name": "segment", "field": "customer.segment"}),
        json!({"id": "i1", "name": "total", "field": "order.total"}),
        json!({"id": "i2", "name": "country", "field": "customer.country"}),
    ];
    if big_input {
        inputs.push(json!({"id": "i3", "name": "items", "field": "order.items"}));
    }
    let rules: Vec<Value> = (0..6)
        .map(|r| {
            let mut rule = json!({"_id": format!("r{r}"), "i0": "", "i1": "", "i2": "", "o0": format!("'{}'", ["block", "review", "step-up", "fast", "watch", "ok"][r])});
            match r {
                0 => rule["i2"] = json!("'NG'"),
                1 => rule["i1"] = json!(format!("> {}", 900_000 + i * 1_000)),
                2 => rule["i0"] = json!("'new'"),
                3 => rule["i0"] = json!("'gold'"),
                4 => rule["i1"] = json!(format!("> {}", 400_000 + i * 5_000)),
                _ => {}
            }
            if big_input {
                rule["i3"] = json!(if r == 1 { "count($) > 1500" } else { "" });
            }
            rule
        })
        .collect();
    json!({"hitPolicy": "first", "passThrough": true, "inputField": null, "outputPath": null, "executionMode": "single",
        "inputs": inputs, "outputs": [{"id": "o0", "name": "flag", "field": format!("checks.n{i}")}], "rules": rules})
}

fn expression(i: usize, pass: bool) -> Value {
    json!({"passThrough": pass, "inputField": null, "outputPath": null, "executionMode": "single", "expressions": [
        {"id": "x0", "key": format!("scores.n{i}"), "value": format!("order.total / {} + customer.history.orders", i + 1)},
        {"id": "x1", "key": format!("seen.n{i}"), "value": "customer.history.returns > 3 or order.channel == 'store'"}
    ]})
}

fn rewrite(i: usize) -> Value {
    json!({"source": format!(
        "export const handler = async (input) => ({{...input, order: {{...input.order, items: input.order.items.map((it) => ({{...it, step{i}: Math.round(it.price * it.qty * {k}) / 100}}))}}}});",
        k = i + 1
    )})
}

fn graph(nodes: Vec<Value>, fan_out: bool) -> Value {
    let side: Vec<String> = nodes
        .iter()
        .filter(|n| n["content"]["executionMode"] == "loop")
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    let mut all = vec![node("request", "inputNode", json!({}))];
    let ids: Vec<String> = nodes
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    all.extend(nodes);
    all.push(node("response", "outputNode", json!({})));
    let mut edges = Vec::new();
    let mut edge = |a: &str, b: &str| {
        edges.push(json!({"id": format!("{a}-{b}"), "type": "edge", "sourceId": a, "targetId": b}))
    };
    if fan_out {
        for id in &ids {
            edge("request", id);
            edge(id, "response");
        }
    } else {
        let mut prev = "request".to_string();
        let mut before = String::new();
        for id in &ids {
            edge(&prev, id);
            if side.contains(&prev) {
                edge(&before, id);
            }
            before = prev;
            prev = id.clone();
        }
        edge(&prev, "response");
    }
    json!({"contentType": "application/vnd.gorules.decision", "nodes": all, "edges": edges})
}

fn big_graphs() -> Vec<(&'static str, Value)> {
    let mixed = |loops: bool| -> Vec<Value> {
        (0..20)
            .map(|i| {
                let id = format!("n{i:02}");
                if loops && i % 5 == 2 {
                    node(&id, "decisionTableNode", table(i, true))
                } else if i % 2 == 0 {
                    node(&id, "decisionTableNode", table(i, false))
                } else {
                    node(&id, "expressionNode", expression(i, true))
                }
            })
            .collect()
    };
    let fan = (0..20)
        .map(|i| {
            let id = format!("n{i:02}");
            if i % 2 == 0 {
                let mut t = table(i, false);
                t["passThrough"] = json!(false);
                node(&id, "decisionTableNode", t)
            } else {
                node(&id, "expressionNode", expression(i, false))
            }
        })
        .collect();
    vec![
        ("ride", graph(mixed(false), false)),
        ("fan", graph(fan, true)),
        ("loops", graph(mixed(true), false)),
        (
            "rewrite",
            graph(
                (0..20)
                    .map(|i| node(&format!("n{i:02}"), "functionNode", rewrite(i)))
                    .collect(),
                false,
            ),
        ),
    ]
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    file: String,
    input: Value,
}

struct Scenario {
    name: String,
    engine: DecisionEngine,
    key: String,
    input: Value,
    iters: usize,
    policy: bool,
}

struct Sample {
    eval_peak: usize,
    eval_ns: f64,
    json_ns: f64,
    peak: usize,
    bytes: usize,
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

async fn measure(s: &Scenario, mode: Mode) -> Sample {
    let options = mode.options();
    let input: Variable = s.input.clone().into();
    for _ in 0..s.iters.min(3) {
        let r = s
            .engine
            .evaluate_with_opts(&s.key, input.depth_clone(64), options)
            .await;
        let _ = r
            .map(|r| serde_json::to_string(&r).unwrap())
            .map_err(|e| e.to_string())
            .unwrap();
    }
    let (mut eval, mut json, mut peaks) = (Vec::new(), Vec::new(), Vec::new());
    let mut eval_peaks = Vec::new();
    let mut bytes = 0;
    for _ in 0..s.iters {
        let context = input.depth_clone(64);
        let base = CURRENT.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t0 = Instant::now();
        let response = s
            .engine
            .evaluate_with_opts(&s.key, context, options)
            .await
            .unwrap();
        let t1 = Instant::now();
        eval_peaks.push(PEAK.load(Ordering::Relaxed).saturating_sub(base) as f64);
        let text = serde_json::to_string(&response).unwrap();
        let t2 = Instant::now();
        peaks.push(PEAK.load(Ordering::Relaxed).saturating_sub(base) as f64);
        bytes = text.len();
        drop(text);
        drop(response);
        eval.push((t1 - t0).as_nanos() as f64);
        json.push((t2 - t1).as_nanos() as f64);
    }
    Sample {
        eval_ns: median(eval),
        json_ns: median(json),
        peak: median(peaks) as usize,
        eval_peak: median(eval_peaks) as usize,
        bytes,
    }
}

fn fmt_ns(ns: f64) -> String {
    if ns >= 1e9 {
        format!("{:.2} s", ns / 1e9)
    } else if ns >= 1e6 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.1} µs", ns / 1e3)
    }
}

fn fmt_bytes(b: usize) -> String {
    let b = b as f64;
    if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.1} KB", b / 1e3)
    } else {
        format!("{b:.0} B")
    }
}

fn rows_of(s: &Scenario, rows: usize) -> Vec<Variable> {
    (0..rows).map(|_| Variable::from(s.input.clone())).collect()
}

enum Owned {
    Dec(Vec<rust_decimal::Decimal>),
    Strs(Vec<&'static str>),
    Any(Vec<Variable>),
}

fn leaves(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(map) if !map.is_empty() => {
            for (k, x) in map {
                let path = match prefix.is_empty() {
                    true => k.clone(),
                    false => format!("{prefix}.{k}"),
                };
                leaves(&path, x, out);
            }
        }
        other => out.push((prefix.to_string(), other.clone())),
    }
}

fn owned_columns(input: &Value, rows: usize) -> Vec<(&'static str, Owned)> {
    let mut flat = Vec::new();
    leaves("", input, &mut flat);
    flat.into_iter()
        .map(|(path, value)| {
            let path: &'static str = Box::leak(path.into_boxed_str());
            let owned = match &value {
                Value::Number(n) => {
                    Owned::Dec(vec![
                        rust_decimal::Decimal::from_str_exact(&n.to_string())
                            .unwrap_or_default();
                        rows
                    ])
                }
                Value::String(t) => {
                    let t: &'static str = Box::leak(t.clone().into_boxed_str());
                    Owned::Strs(vec![t; rows])
                }
                other => Owned::Any((0..rows).map(|_| Variable::from(other.clone())).collect()),
            };
            (path, owned)
        })
        .collect()
}

fn columns_of<'a>(owned: &'a [(&'static str, Owned)], rows: usize) -> Columns<'a> {
    let mut columns = Columns::new(rows);
    for (path, values) in owned {
        let values = match values {
            Owned::Dec(d) => Values::Dec(d),
            Owned::Strs(s) => Values::Strs(s),
            Owned::Any(a) => Values::Any(a),
        };
        columns = columns.column(path, Column::new(values));
    }
    columns
}

struct BatchSample {
    ns: f64,
    json_ns: f64,
    peak: usize,
    bytes: usize,
}

async fn batch(s: &Scenario, mode: Mode, rows: usize, reps: usize) -> BatchSample {
    let options = mode.options();
    let (mut times, mut json, mut peaks) = (Vec::new(), Vec::new(), Vec::new());
    let mut last = 0usize;
    for rep in 0..reps + 1 {
        let contexts = rows_of(s, rows);
        let base = CURRENT.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t0 = Instant::now();
        let results = s.engine.evaluate_batch(&s.key, &contexts, options).await;
        let t1 = Instant::now();
        if let (Ok(dir), 0, Some(Ok(first))) = (std::env::var("TRACE_DUMP"), rep, results.first()) {
            let slug: String = format!("{} {}", s.name, mode.name())
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            std::fs::create_dir_all(&dir).unwrap();
            let value = serde_json::to_value(first).unwrap();
            let text = serde_json::to_string(&value["trace"]).unwrap();
            std::fs::write(Path::new(&dir).join(format!("{slug}-batch.json")), text).unwrap();
        }
        let mut bytes = 0usize;
        for r in &results {
            bytes += match r {
                Ok(r) => serde_json::to_string(r).unwrap().len(),
                Err(e) => e.to_string().len(),
            };
        }
        std::hint::black_box(bytes);
        let t2 = Instant::now();
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
        last = bytes;
        drop(results);
        if rep > 0 {
            times.push((t1 - t0).as_nanos() as f64);
            json.push((t2 - t1).as_nanos() as f64);
            peaks.push(peak as f64);
        }
    }
    BatchSample {
        ns: median(times),
        json_ns: median(json),
        peak: median(peaks) as usize,
        bytes: last / rows.max(1),
    }
}

async fn columnar(s: &Scenario, mode: Mode, rows: usize, reps: usize) -> Option<BatchSample> {
    let decision = s.engine.get_decision(&s.key).await.ok()?.ok()?;
    let owned = owned_columns(&s.input, rows);
    let columns = columns_of(&owned, rows);
    let options = mode.options();
    let (mut times, mut json, mut peaks) = (Vec::new(), Vec::new(), Vec::new());
    let mut last = 0usize;
    for rep in 0..reps + 1 {
        let base = CURRENT.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let t0 = Instant::now();
        let output = decision.evaluate_columns(&columns, options).await;
        let t1 = Instant::now();
        let bytes = columnar_json(&output, mode);
        std::hint::black_box(bytes);
        let t2 = Instant::now();
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
        last = bytes;
        drop(output);
        if rep > 0 {
            times.push((t1 - t0).as_nanos() as f64);
            json.push((t2 - t1).as_nanos() as f64);
            peaks.push(peak as f64);
        }
    }
    Some(BatchSample {
        ns: median(times),
        json_ns: median(json),
        peak: median(peaks) as usize,
        bytes: last / rows.max(1),
    })
}

#[cfg(feature = "base")]
fn columnar_json(output: &zen_engine::ColumnarOutput, _mode: Mode) -> usize {
    (0..output.rows)
        .map(|row| serde_json::to_string(&output.row(row)).unwrap().len())
        .sum()
}

#[cfg(not(feature = "base"))]
fn columnar_json(output: &zen_engine::ColumnarOutput, _mode: Mode) -> usize {
    (0..output.rows)
        .map(|row| {
            serde_json::to_string(&output.row(row)).unwrap().len()
                + output.trace_json(row).map_or(0, |t| t.len())
        })
        .sum()
}

#[cfg(feature = "base")]
fn reevaluations() -> Vec<(&'static str, usize)> {
    Vec::new()
}

#[cfg(not(feature = "base"))]
fn reevaluations() -> Vec<(&'static str, usize)> {
    zen_engine::trace::testing::reevaluations()
}

fn report_reevaluated(label: &str, before: &[(&'static str, usize)], runs: usize) {
    let after = reevaluations();
    let moved: Vec<String> = after
        .iter()
        .filter_map(|(k, v)| {
            let was = before.iter().find(|(b, _)| b == k).map_or(0, |(_, n)| *n);
            (*v > was).then(|| format!("{k}: {} rows", (v - was) / runs.max(1)))
        })
        .collect();
    if !moved.is_empty() {
        eprintln!("{label}: re-evaluated {}", moved.join(", "));
    }
}

async fn batches(scenarios: &[Scenario], filter: Option<&str>, rows: usize) {
    println!("| scenario | rows | path | mode | batch | rows/s | + JSON per row | peak memory | JSON bytes per row |");
    println!("|---|---:|---|---|---:|---:|---:|---:|---:|");
    for s in scenarios {
        if s.policy || filter.is_some_and(|f| !s.name.contains(f)) {
            continue;
        }
        let rows = match s.iters < 100 {
            true => (rows / 100).max(4),
            false => rows,
        };
        let reps = std::env::var("TRACE_REPS")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(match s.iters < 100 {
                true => 3,
                false => 10,
            });
        let wanted = |mode: Mode| {
            std::env::var("TRACE_MODE").map_or(true, |m| mode.name().contains(m.as_str()))
        };
        let paths = std::env::var("TRACE_PATH").unwrap_or_default();
        for mode in Mode::all() {
            if !wanted(mode) || paths == "columnar" {
                continue;
            }
            let before = reevaluations();
            let r = batch(s, mode, rows, reps).await;
            report_reevaluated(
                &format!("{} batch {}", s.name, mode.name()),
                &before,
                reps + 1,
            );
            println!(
                "| {} | {rows} | batch | {} | {} | {:.0} | {} | {} | {} |",
                s.name,
                mode.name(),
                fmt_ns(r.ns),
                rows as f64 / (r.ns / 1e9),
                fmt_ns(r.json_ns / rows as f64),
                fmt_bytes(r.peak),
                r.bytes
            );
        }
        for mode in Mode::all() {
            #[cfg(feature = "base")]
            if mode != Mode::Off {
                continue;
            }
            if !wanted(mode) || paths == "batch" {
                continue;
            }
            let before = reevaluations();
            let Some(r) = columnar(s, mode, rows, reps).await else {
                continue;
            };
            report_reevaluated(
                &format!("{} columnar {}", s.name, mode.name()),
                &before,
                reps + 1,
            );
            println!(
                "| {} | {rows} | columnar | {} | {} | {:.0} | {} | {} | {} |",
                s.name,
                mode.name(),
                fmt_ns(r.ns),
                rows as f64 / (r.ns / 1e9),
                fmt_ns(r.json_ns / rows as f64),
                fmt_bytes(r.peak),
                r.bytes
            );
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let manifest_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../manifest.json".to_string());
    ZEN_CONFIG.compiled.store(
        std::env::var("BENCH_ENGINE").map_or(true, |e| e != "interpreted"),
        Ordering::Relaxed,
    );
    let filter = std::env::var("TRACE_BENCH").ok();
    let small_iters: usize = std::env::var("SMALL_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let big_iters: usize = std::env::var("BIG_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let items: usize = std::env::var("BIG_ITEMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    let manifest_path = Path::new(&manifest_path);
    let fixtures = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("fixtures");
    let manifest: Vec<Entry> =
        serde_json::from_str(&std::fs::read_to_string(manifest_path).expect("manifest"))
            .expect("manifest json");
    let files = match std::env::var("BENCH_LOADER").is_ok_and(|l| l == "filesystem") {
        true => DecisionEngine::default().with_loader(Arc::new(FilesystemLoader::new(
            FilesystemLoaderOptions {
                root: fixtures.to_string_lossy().to_string(),
            },
        ))),
        false => {
            let memory = Arc::new(MemoryLoader::default());
            for e in &manifest {
                let text = std::fs::read_to_string(fixtures.join(&e.file)).expect("fixture");
                memory.add(
                    e.file.as_str(),
                    serde_json::from_str::<DecisionContent>(&text).expect("fixture json"),
                );
            }
            DecisionEngine::default().with_loader(memory)
        }
    };
    let mut scenarios: Vec<Scenario> = manifest
        .iter()
        .filter(|e| e.file.starts_with("graphs/") || e.file.starts_with("policies/"))
        .map(|e| Scenario {
            name: format!(
                "{} {}",
                if e.file.starts_with("graphs/") {
                    "small"
                } else {
                    "policy"
                },
                e.name
            ),
            engine: files.clone(),
            key: e.file.clone(),
            input: e.input.clone(),
            iters: small_iters,
            policy: e.file.starts_with("policies/"),
        })
        .collect();
    let loader = Arc::new(MemoryLoader::default());
    for (key, graph) in big_graphs() {
        loader.add(
            key,
            serde_json::from_value::<DecisionContent>(graph).unwrap(),
        );
    }
    let big = DecisionEngine::default().with_loader(loader);
    scenarios.push(Scenario {
        name: "loops, 50 items".to_string(),
        engine: big.clone(),
        key: "loops".to_string(),
        input: payload(1, 50),
        iters: small_iters / 10,
        policy: false,
    });
    for key in ["ride", "fan", "loops", "rewrite"] {
        scenarios.push(Scenario {
            name: format!("{key}, {items} items"),
            engine: big.clone(),
            key: key.to_string(),
            input: payload(1, items),
            iters: big_iters,
            policy: false,
        });
    }
    if let Ok(rows) = std::env::var("TRACE_BATCH") {
        batches(&scenarios, filter.as_deref(), rows.parse().unwrap_or(1000)).await;
        return;
    }
    if let Ok(dir) = std::env::var("TRACE_SIZES") {
        sizes(&scenarios, filter.as_deref(), &dir).await;
        return;
    }
    println!("| scenario | mode | evaluate | + JSON | peak memory | peak during evaluate | response JSON |");
    println!("|---|---|---:|---:|---:|---:|---:|");
    for s in &scenarios {
        if filter
            .as_ref()
            .is_some_and(|f| !s.name.contains(f.as_str()))
        {
            continue;
        }
        for mode in Mode::all() {
            if std::env::var("TRACE_MODE").is_ok_and(|m| !mode.name().contains(m.as_str())) {
                continue;
            }
            let r = measure(s, mode).await;
            println!(
                "| {} | {} | {} | {} | {} | {} | {} |",
                s.name,
                mode.name(),
                fmt_ns(r.eval_ns),
                fmt_ns(r.json_ns),
                fmt_bytes(r.peak),
                fmt_bytes(r.eval_peak),
                fmt_bytes(r.bytes)
            );
        }
    }
}

async fn sizes(scenarios: &[Scenario], filter: Option<&str>, dir: &str) {
    std::fs::create_dir_all(dir).unwrap();
    println!("| scenario | mode | trace JSON | zstd |");
    println!("|---|---|---:|---:|");
    for s in scenarios {
        if filter.is_some_and(|f| !s.name.contains(f)) {
            continue;
        }
        for mode in Mode::all() {
            if mode == Mode::Off {
                continue;
            }
            let options = mode.options();
            let input: Variable = s.input.clone().into();
            let response = s
                .engine
                .evaluate_with_opts(&s.key, input, options)
                .await
                .unwrap();
            let value = serde_json::to_value(&response).unwrap();
            let text = serde_json::to_string(&value["trace"]).unwrap();
            let packed = zstd::encode_all(text.as_bytes(), 3).unwrap();
            let slug: String = format!("{} {}", s.name, mode.name())
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            std::fs::write(Path::new(dir).join(format!("{slug}.json")), &text).unwrap();
            println!(
                "| {} | {} | {} | {} |",
                s.name,
                mode.name(),
                text.len(),
                packed.len()
            );
        }
    }
}

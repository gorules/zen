mod support;
#[path = "support/fixture.rs"]
mod fixture;

use fixture::{Fixture, NETWORK_BOUND, TIME_BOUND};
#[cfg(feature = "driver")]
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use support::columns::Built;
use support::golden::{Canon, Live, Reference};
#[cfg(feature = "driver")]
use zen_engine::model::GraphContent;
use zen_engine::{Decision, EvaluationOptions};
use zen_expression::Variable;

struct Outcome;

impl Outcome {
    const CLOCK: &'static str = "2025-08-19T16:55:02.078Z";

    fn of(result: Result<zen_engine::DecisionGraphResponse, Box<zen_engine::EvaluationError>>) -> Result<String, String> {
        result
            .map(|r| serde_json::to_string(&r.result).unwrap_or_default())
            .map_err(|e| serde_json::to_value(&*e).map(|v| v.to_string()).unwrap_or_else(|_| e.to_string()))
    }

    fn recorded(outcome: &support::golden::Outcome) -> Result<String, String> {
        match outcome {
            support::golden::Outcome::Ok(value) => Ok(Canon::value(Canon::restore(value).to_value()).to_string()),
            support::golden::Outcome::Err { value, .. } => Err(value.to_string()),
        }
    }

    fn canonical(result: Result<String, String>) -> Result<String, String> {
        let canon = |text: String| serde_json::from_str::<Value>(&text).map(|v| Canon::value(v).to_string()).unwrap_or(text);
        result.map(canon).map_err(canon)
    }

    fn columnar(outcome: &support::golden::Outcome) -> Result<Value, String> {
        match outcome {
            support::golden::Outcome::Ok(value) => Ok(Built::normalized(Canon::restore(value).to_value())),
            support::golden::Outcome::Err { value, .. } => Err(value.to_string()),
        }
    }
}

#[tokio::test]
async fn compiled_graphs_match_the_walker() {
    if Reference::unavailable("graph-fixtures") {
        return;
    }
    Reference::prepare(Outcome::CLOCK);
    let reference = Reference::open("graph-fixtures");
    let mut census: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0usize;
    for fixture in Fixture::load_all() {
        let walker = Live::walker(&Decision::from(Arc::new(fixture.content.clone())));
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        let verdict = compiled.compiled_verdict();
        let key = match &verdict {
            Some(Ok(())) => "compiled".to_string(),
            Some(Err(reason)) => format!("walker: {reason}"),
            None => "not compiled".to_string(),
        };
        census.entry(key).or_default().push(fixture.name.clone());
        if !matches!(verdict, Some(Ok(()))) || NETWORK_BOUND.contains(&fixture.name.as_str()) {
            continue;
        }
        let variants = fixture.variants();
        let contexts: Vec<Variable> = variants.iter().map(|v| Variable::from(v.clone())).collect();
        let options = EvaluationOptions::default();
        let batch = compiled.evaluate_batch(&contexts, options).await;
        for (i, input) in variants.iter().enumerate() {
            let context = Variable::from(input.clone());
            let want = reference
                .graph(
                    || Canon::key(&fixture.name, &[], &context),
                    || async {
                        Live::require_walker();
                        Outcome::of(walker.evaluate(Variable::from(input.clone())).await)
                    },
                    Outcome::recorded,
                )
                .await;
            let single = Outcome::of(compiled.evaluate(Variable::from(input.clone())).await);
            let batched = batch.get(i).map(|r| match r {
                Ok(r) => Ok(serde_json::to_string(&r.result).unwrap_or_default()),
                Err(e) => Err(serde_json::to_value(&**e).map(|v| v.to_string()).unwrap_or_else(|_| e.to_string())),
            });
            let (single, batched) = match reference.is_live() {
                true => (single, batched),
                false => (Outcome::canonical(single), batched.map(Outcome::canonical)),
            };
            compared += 1;
            if single != want {
                failures.push(format!("{} single {input}\n  walker   {want:?}\n  compiled {single:?}", fixture.name));
            }
            if batched.as_ref() != Some(&want) {
                failures.push(format!("{} batch {input}\n  walker   {want:?}\n  compiled {batched:?}", fixture.name));
            }
        }
    }
    for (verdict, files) in &census {
        println!("{verdict}: {} {:?}", files.len(), files);
    }
    println!("compared {compared} inputs");
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

#[cfg(feature = "driver")]
#[tokio::test]
#[ignore]
async fn compiled_graph_throughput() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let filter = std::env::var("BENCH_FILE").ok();
    let mut ratios: Vec<(f64, f64, String)> = Vec::new();
    println!("{:44} {:>10} {:>10} {:>10} {:>7} {:>7}", "fixture", "walker", "single", "batch", "x1", "xN");
    for fixture in Fixture::load_all() {
        if NETWORK_BOUND.contains(&fixture.name.as_str())
            || TIME_BOUND.contains(&fixture.name.as_str())
            || filter.as_ref().is_some_and(|f| !fixture.name.contains(f.as_str()))
        {
            continue;
        }
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        if !matches!(compiled.compiled_verdict(), Some(Ok(()))) {
            continue;
        }
        let walker = compiled.interpreted();
        let variants = fixture.variants();
        let contexts: Vec<Variable> = (0..rows).map(|i| Variable::from(variants[i % variants.len()].clone())).collect();
        let options = EvaluationOptions::default();
        if let Ok(seconds) = std::env::var("BENCH_PROFILE") {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(5));
            let single = std::env::var("BENCH_MODE").is_ok_and(|m| m == "single");
            while std::time::Instant::now() < until {
                match single {
                    true => {
                        for c in &contexts {
                            let _ = compiled.evaluate(c.clone()).await;
                        }
                    }
                    false => {
                        let _ = compiled.evaluate_batch(&contexts, options).await;
                    }
                }
            }
            continue;
        }
        let time = |elapsed: std::time::Duration, n: usize| elapsed.as_nanos() as f64 / n as f64;
        let mut best = [f64::MAX; 3];
        for _ in 0..5 {
            let start = std::time::Instant::now();
            for c in &contexts {
                let _ = walker.evaluate(c.clone()).await;
            }
            best[0] = best[0].min(time(start.elapsed(), rows));
            let start = std::time::Instant::now();
            for c in &contexts {
                let _ = compiled.evaluate(c.clone()).await;
            }
            best[1] = best[1].min(time(start.elapsed(), rows));
            let start = std::time::Instant::now();
            let _ = compiled.evaluate_batch(&contexts, options).await;
            best[2] = best[2].min(time(start.elapsed(), rows));
        }
        println!("{:44} {:>10.0} {:>10.0} {:>10.0} {:>6.2}x {:>6.2}x", fixture.name, best[0], best[1], best[2], best[0] / best[1], best[0] / best[2]);
        ratios.push((best[0] / best[1], best[0] / best[2], fixture.name.clone()));
    }
    let gm = |f: fn(&(f64, f64, String)) -> f64| (ratios.iter().map(|r| f(r).ln()).sum::<f64>() / ratios.len() as f64).exp();
    let median = |f: fn(&(f64, f64, String)) -> f64| {
        let mut v: Vec<f64> = ratios.iter().map(f).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    println!("single: geomean {:.2}x median {:.2}x | batch: geomean {:.2}x median {:.2}x", gm(|r| r.0), median(|r| r.0), gm(|r| r.1), median(|r| r.1));
}


#[tokio::test]
async fn columnar_graphs_match_the_walker() {
    if Reference::unavailable("graph-fixtures") {
        return;
    }
    Reference::prepare(Outcome::CLOCK);
    let reference = Reference::open("graph-fixtures");
    let mut failures: Vec<String> = Vec::new();
    let (mut compared, mut skipped) = (0usize, 0usize);
    for fixture in Fixture::load_all() {
        if NETWORK_BOUND.contains(&fixture.name.as_str()) || TIME_BOUND.contains(&fixture.name.as_str()) {
            continue;
        }
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        if !matches!(compiled.compiled_verdict(), Some(Ok(()))) {
            continue;
        }
        let walker = Live::walker(&compiled);
        let variants = fixture.variants();
        let Some(built) = Built::new(&variants) else {
            skipped += 1;
            continue;
        };
        let cycled: Vec<Value> = (0..variants.len() * 4).map(|i| variants[i % variants.len()].clone()).collect();
        for built in std::iter::once(built).chain(Built::lists(&variants)).chain(Built::new(&cycled)).chain(Built::nested(&variants)).chain(Built::nested(&cycled)) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            let output = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
            for row in 0..built.rows {
                let input = columns.row(row);
                let want = reference
                    .graph(
                        || Canon::key(&fixture.name, &[], &input),
                        || async {
                            Live::require_walker();
                            match walker.evaluate(input.clone()).await {
                                Ok(r) => Ok(Built::normalized(r.result.to_value())),
                                Err(e) => Err(serde_json::to_value(&*e).map(|v| v.to_string()).unwrap_or_default()),
                            }
                        },
                        Outcome::columnar,
                    )
                    .await;
                let got = match &output.errors[row] {
                    Some(e) => Err(serde_json::to_value(&**e).map(|v| v.to_string()).unwrap_or_default()),
                    None => Ok(Built::normalized(output.row(row).to_value())),
                };
                compared += 1;
                if got != want {
                    failures.push(format!("{} {}\n  walker   {want:?}\n  columnar {got:?}", fixture.name, input.to_value()));
                }
            }
        }
    }
    println!("compared {compared} rows, {skipped} fixtures skipped");
    failures.extend(reference.missing());
    let mut kinds: Vec<&str> = failures.iter().filter_map(|f| f.split_whitespace().next()).collect();
    kinds.dedup();
    assert!(failures.is_empty(), "{} mismatches in {kinds:?}:\n{}", failures.len(), failures.iter().take(15).cloned().collect::<Vec<_>>().join("\n"));
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn struct_inputs_match_the_walker() {
    use zen_expression::lane::{Column, Columns, Values};
    Reference::prepare(Outcome::CLOCK);
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0usize;
    for fixture in Fixture::load_all() {
        if NETWORK_BOUND.contains(&fixture.name.as_str()) || TIME_BOUND.contains(&fixture.name.as_str()) {
            continue;
        }
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        if !matches!(compiled.compiled_verdict(), Some(Ok(()))) {
            continue;
        }
        let walker = compiled.interpreted();
        let variants = fixture.variants();
        let rows: Vec<Value> = (0..variants.len() * 3).map(|i| variants[i % variants.len()].clone()).collect();
        let scalar = |v: &Value| !matches!(v, Value::Object(_) | Value::Array(_));
        let Some(key) = rows
            .iter()
            .filter_map(|r| r.as_object())
            .flat_map(|m| m.iter())
            .find(|(_, v)| v.as_object().is_some_and(|o| !o.is_empty() && o.values().all(scalar)))
            .map(|(k, _)| k.clone())
        else {
            continue;
        };
        let usable = rows.iter().all(|r| r.get(&key).is_none_or(|v| v.is_null() || v.as_object().is_some_and(|o| o.values().all(scalar))));
        if !usable {
            continue;
        }
        let inner: Vec<Value> = rows.iter().map(|r| r.get(&key).filter(|v| v.is_object()).cloned().unwrap_or(json!({}))).collect();
        let rest: Vec<Value> = rows
            .iter()
            .map(|r| {
                let mut r = r.clone();
                if let Some(m) = r.as_object_mut() {
                    m.remove(&key);
                    if m.is_empty() {
                        m.insert("__pad".into(), json!(1));
                    }
                }
                r
            })
            .collect();
        let (Some(inner_built), Some(rest_built)) = (Built::new(&inner), Built::new(&rest)) else {
            continue;
        };
        if inner_built.rows != rows.len() || rest_built.rows != rows.len() || inner_built.paths.iter().any(|p| p.contains('.')) {
            continue;
        }
        let inner_columns = inner_built.columns();
        let fields: Vec<(&str, Column)> = inner_columns.columns.clone();
        let mut valid = vec![0u64; rows.len().div_ceil(64)];
        rows.iter().enumerate().filter(|(_, r)| r.get(&key).is_some_and(Value::is_object)).for_each(|(i, _)| valid[i / 64] |= 1 << (i % 64));
        let rest_columns = rest_built.columns();
        let mut columns = Columns::new(rows.len());
        for (path, column) in rest_columns.columns.iter().filter(|(p, _)| *p != "__pad") {
            columns = columns.column(path, *column);
        }
        columns = columns.column(&key, Column::with_validity(Values::Struct { fields: &fields, len: rows.len() }, &valid, 0));
        let output = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
        for row in 0..rows.len() {
            let input = columns.row(row);
            let want = match walker.evaluate(input.clone()).await {
                Ok(r) => Ok(Built::normalized(r.result.to_value())),
                Err(e) => Err(serde_json::to_value(&*e).map(|v| v.to_string()).unwrap_or_default()),
            };
            let got = match &output.errors[row] {
                Some(e) => Err(serde_json::to_value(&**e).map(|v| v.to_string()).unwrap_or_default()),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            compared += 1;
            if got != want {
                failures.push(format!("{} {}\n  walker   {want:?}\n  columnar {got:?}", fixture.name, input.to_value()));
            }
        }
    }
    println!("struct inputs: compared {compared} rows");
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}

#[tokio::test]
async fn columnar_tables_match_coded_nulls_against_null_cells() {
    use zen_expression::lane::{Column, Columns, Dictionary, Values};
    let table = |inputs: Value, first: Value| -> Decision {
        let content: zen_engine::model::GraphContent = serde_json::from_value(serde_json::json!({
            "nodes": [
                { "id": "in", "name": "in", "type": "inputNode", "content": {} },
                {
                    "id": "cls", "name": "cls", "type": "decisionTableNode",
                    "content": {
                        "hitPolicy": "first",
                        "inputs": inputs,
                        "outputs": [{ "id": "o1", "name": "cls", "field": "cls" }],
                        "rules": [first, { "_id": "r2", "o1": "'other'" }]
                    }
                },
                { "id": "out", "name": "out", "type": "outputNode", "content": {} }
            ],
            "edges": [
                { "id": "e1", "sourceId": "in", "targetId": "cls" },
                { "id": "e2", "sourceId": "cls", "targetId": "out" }
            ]
        }))
        .expect("graph");
        let mut decision = Decision::from(Arc::new(content));
        decision.compile();
        decision
    };
    let dispatched = table(serde_json::json!([{ "id": "i1", "name": "k", "field": "k" }]), serde_json::json!({ "_id": "r1", "i1": "null", "o1": "'null'" }));
    let generic = table(
        serde_json::json!([{ "id": "i1", "name": "k", "field": "k" }, { "id": "i2", "name": "always" }]),
        serde_json::json!({ "_id": "r1", "i1": "null", "i2": "true", "o1": "'null'" }),
    );
    let keys = [0i32, -1, 0, -1, 1];
    let column = Column::new(Values::Dict {
        keys: &keys,
        values: Dictionary::Text { offsets: &[0, 1, 1], data: "x" },
    });
    let columns = Columns::new(keys.len()).column("k", column);
    let want = ["other", "null", "other", "null", "other"];
    for (label, decision) in [("dispatched", &dispatched), ("generic", &generic)] {
        let output = decision.evaluate_columns(&columns, EvaluationOptions::default()).await;
        for (row, want) in want.iter().enumerate() {
            assert!(output.errors[row].is_none(), "{label} row {row}: {:?}", output.errors[row]);
            assert_eq!(output.row(row).to_value(), serde_json::json!({ "cls": want }), "{label} row {row}");
        }
    }
}

#[tokio::test]
async fn columnar_function_rows_time_out_independently() {
    let fixture = Fixture::load_all().into_iter().find(|f| f.name == "infinite-function.json").expect("fixture");
    let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
    compiled.compile();
    let rows: Vec<Value> = (0..3).map(|i| serde_json::json!({ "row": i })).collect();
    let built = Built::new(&rows).expect("columns");
    let columns = built.columns();
    let started = std::time::Instant::now();
    let output = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
    let elapsed = started.elapsed();
    let messages: Vec<String> = output
        .errors
        .iter()
        .map(|e| e.as_ref().map(|e| e.to_string()).unwrap_or_default())
        .collect();
    assert!(messages.iter().all(|m| m.contains("interrupted")), "{messages:?}");
    assert!(elapsed < std::time::Duration::from_secs(30), "{elapsed:?}");
}

#[cfg(feature = "driver")]
#[tokio::test]
#[ignore]
async fn columnar_graph_throughput() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let filter = std::env::var("BENCH_FILE").ok();
    let read = std::env::var("BENCH_READ").is_ok_and(|v| v == "1");
    let loader = std::env::var("BENCH_LOADER").is_ok_and(|v| v == "1");
    let mut ratios: Vec<(f64, String)> = Vec::new();
    for fixture in Fixture::load_all() {
        if NETWORK_BOUND.contains(&fixture.name.as_str())
            || TIME_BOUND.contains(&fixture.name.as_str())
            || filter.as_ref().is_some_and(|f| !fixture.name.contains(f.as_str()))
        {
            continue;
        }
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        if loader {
            compiled = compiled.with_loader(Arc::new(support::create_fs_loader()));
        }
        compiled.compile();
        if !matches!(compiled.compiled_verdict(), Some(Ok(()))) {
            continue;
        }
        let walker = compiled.interpreted();
        let variants = match std::env::var("BENCH_VALID").is_ok() && !fixture.inputs.is_empty() {
            true => fixture.inputs.clone(),
            false => fixture.typed_variants(),
        };
        let inputs: Vec<Value> = (0..rows).map(|i| variants[i % variants.len()].clone()).collect();
        let built = match (std::env::var("BENCH_NESTED").is_ok(), std::env::var("BENCH_LISTS").is_ok()) {
            (true, _) => Built::nested(&inputs).or_else(|| Built::lists(&inputs)).or_else(|| Built::new(&inputs)),
            (false, true) => Built::lists(&inputs).or_else(|| Built::new(&inputs)),
            (false, false) => Built::new(&inputs),
        };
        let Some(built) = built else {
            continue;
        };
        let fields = built.fields();
        let structs = built.structs(&fields);
        let columns = built.columns_with(&structs);
        let contexts: Vec<Variable> = (0..rows).map(|r| columns.row(r)).collect();
        let options = EvaluationOptions::default();
        if let Ok(seconds) = std::env::var("BENCH_PROFILE") {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(5));
            while std::time::Instant::now() < until {
                let _ = compiled.evaluate_columns(&columns, options).await;
            }
            continue;
        }
        let mut best = [f64::MAX; 2];
        for _ in 0..5 {
            let start = std::time::Instant::now();
            for c in contexts.iter().take(256) {
                let _ = walker.evaluate(c.clone()).await;
            }
            best[0] = best[0].min(start.elapsed().as_nanos() as f64 / rows.min(256) as f64);
            let start = std::time::Instant::now();
            let output = compiled.evaluate_columns(&columns, options).await;
            if read {
                output.columns.iter().for_each(|(_, column)| {
                    std::hint::black_box(column.column());
                });
            }
            drop(output);
            best[1] = best[1].min(start.elapsed().as_nanos() as f64 / rows as f64);
        }
        println!("{:44} {:>10.0} {:>10.1} {:>7.1}x", fixture.name, best[0], best[1], best[0] / best[1]);
        ratios.push((best[0] / best[1], fixture.name.clone()));
    }
    let mut sorted: Vec<f64> = ratios.iter().map(|r| r.0).collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let gm = (sorted.iter().map(|r| r.ln()).sum::<f64>() / sorted.len() as f64).exp();
    println!(
        "columnar: geomean {gm:.1}x median {:.1}x p10 {:.1}x p90 {:.1}x",
        sorted[sorted.len() / 2],
        sorted[sorted.len() / 10],
        sorted[sorted.len() * 9 / 10]
    );
}

#[cfg(feature = "driver")]
struct Latency;

#[cfg(feature = "driver")]
impl Latency {
    async fn measure<F, Fut>(contexts: &[Variable], reps: usize, mut f: F) -> f64
    where
        F: FnMut(Variable) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let mut best = f64::MAX;
        for _ in 0..reps {
            let fresh: Vec<Variable> = contexts.iter().map(|c| c.depth_clone(usize::MAX)).collect();
            let start = std::time::Instant::now();
            for context in fresh {
                f(context).await;
            }
            best = best.min(start.elapsed().as_nanos() as f64 / contexts.len() as f64);
        }
        best
    }

    fn geomean(values: &[f64]) -> f64 {
        (values.iter().map(|v| v.ln()).sum::<f64>() / values.len().max(1) as f64).exp()
    }
}

#[cfg(feature = "driver")]
#[tokio::test]
#[ignore]
async fn single_row_latency() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", Outcome::CLOCK);
    let reps: usize = std::env::var("BENCH_REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
    let filter = std::env::var("BENCH_FILE").ok();
    let traced = EvaluationOptions { trace: true, max_depth: 10 };
    let mut columns: [Vec<f64>; 5] = Default::default();
    println!("{:44} {:>10} {:>10} {:>10} {:>10} {:>10}", "fixture", "compiled", "cold", "traced", "walker", "w-traced");
    for fixture in Fixture::load_all() {
        if NETWORK_BOUND.contains(&fixture.name.as_str())
            || TIME_BOUND.contains(&fixture.name.as_str())
            || filter.as_ref().is_some_and(|f| !fixture.name.contains(f.as_str()))
        {
            continue;
        }
        let content = Arc::new(fixture.content.clone());
        let mut compiled = Decision::from(content.clone());
        compiled.compile();
        if !matches!(compiled.compiled_verdict(), Some(Ok(()))) {
            continue;
        }
        let walker = compiled.interpreted();
        let contexts: Vec<Variable> = fixture.variants().into_iter().take(64).map(Variable::from).collect();
        let times = [
            Latency::measure(&contexts, reps, |c| {
                let compiled = &compiled;
                async move { drop(compiled.evaluate(c).await) }
            })
            .await,
            Latency::measure(&contexts, reps, |c| {
                let content = content.clone();
                async move { drop(Decision::from(content).evaluate(c).await) }
            })
            .await,
            Latency::measure(&contexts, reps, |c| {
                let compiled = &compiled;
                async move { drop(compiled.evaluate_with_opts(c, traced).await) }
            })
            .await,
            Latency::measure(&contexts, reps, |c| {
                let walker = &walker;
                async move { drop(walker.evaluate(c).await) }
            })
            .await,
            Latency::measure(&contexts, reps, |c| {
                let walker = &walker;
                async move { drop(walker.evaluate_with_opts(c, traced).await) }
            })
            .await,
        ];
        println!("{:44} {:>10.0} {:>10.0} {:>10.0} {:>10.0} {:>10.0}", fixture.name, times[0], times[1], times[2], times[3], times[4]);
        columns.iter_mut().zip(times).for_each(|(column, t)| column.push(t));
    }
    let gm: Vec<f64> = columns.iter().map(|c| Latency::geomean(c)).collect();
    println!("geomean ns/eval ({} fixtures): compiled {:.0} cold {:.0} traced {:.0} walker {:.0} walker-traced {:.0}", columns[0].len(), gm[0], gm[1], gm[2], gm[3], gm[4]);
}

#[cfg(feature = "driver")]
struct Noise(u64);

#[cfg(feature = "driver")]
impl Noise {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

#[cfg(feature = "driver")]
struct Wide {
    numbers: Vec<rust_decimal::Decimal>,
    strings: Vec<String>,
}

#[cfg(feature = "driver")]
impl Wide {
    fn of(fixture: &Fixture) -> Wide {
        let text = serde_json::to_string(&fixture.content).unwrap_or_default();
        let mut numbers: Vec<rust_decimal::Decimal> = Vec::new();
        let mut strings: Vec<String> = Vec::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'\'' {
                let end = text[i + 1..].find('\'').map(|e| i + 1 + e);
                if let Some(end) = end {
                    let s = &text[i + 1..end];
                    if s.len() <= 40 && !strings.iter().any(|x| x == s) {
                        strings.push(s.to_string());
                    }
                    i = end + 1;
                    continue;
                }
            }
            if c.is_ascii_digit() && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')) {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                if let Ok(n) = text[start..i].trim_end_matches('.').parse::<rust_decimal::Decimal>() {
                    if !numbers.contains(&n) && numbers.len() < 64 {
                        numbers.push(n);
                    }
                }
                continue;
            }
            i += 1;
        }
        for input in &fixture.inputs {
            let mut leaves = Vec::new();
            Self::leaves(input, &mut leaves);
            for leaf in leaves {
                match leaf {
                    Value::String(s) if !strings.contains(&s) => strings.push(s),
                    Value::Number(n) => {
                        if let Ok(d) = n.to_string().parse::<rust_decimal::Decimal>() {
                            if !numbers.contains(&d) {
                                numbers.push(d);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Wide { numbers, strings }
    }

    fn leaves(value: &Value, out: &mut Vec<Value>) {
        match value {
            Value::Object(map) => map.values().for_each(|v| Self::leaves(v, out)),
            Value::Array(items) => items.iter().for_each(|v| Self::leaves(v, out)),
            other => out.push(other.clone()),
        }
    }

    fn number(&self, noise: &mut Noise, current: &serde_json::Number) -> Value {
        let base = match (noise.chance(70), self.numbers.is_empty()) {
            (true, false) => self.numbers[noise.below(self.numbers.len())],
            _ => current.to_string().parse().unwrap_or_default(),
        };
        let deltas = ["0", "0", "1", "-1", "0.5", "-0.01", "0.001"];
        let delta: rust_decimal::Decimal = deltas[noise.below(deltas.len())].parse().unwrap_or_default();
        let mut value = base.checked_add(delta).unwrap_or(base);
        match noise.below(6) {
            0 => value = value.checked_mul(rust_decimal::Decimal::from(10)).unwrap_or(value),
            1 => value = -value,
            2 => value.rescale(value.scale() + 2),
            _ => {}
        }
        if value.is_integer() && noise.chance(50) {
            value = value.trunc();
            value.rescale(0);
        }
        serde_json::from_str(&value.to_string()).unwrap_or(Value::Null)
    }

    fn string(&self, noise: &mut Noise, current: &str) -> Value {
        match noise.below(10) {
            0..=5 if !self.strings.is_empty() => json!(self.strings[noise.below(self.strings.len())]),
            6 => json!(current),
            7 => json!(""),
            8 => json!(format!("unseen{}", noise.below(1000))),
            _ => json!(current.to_uppercase()),
        }
    }

    fn mutate(&self, value: &Value, noise: &mut Noise, depth: usize) -> Option<Value> {
        if depth > 0 && noise.chance(6) {
            return None;
        }
        if depth > 0 && noise.chance(4) {
            return Some(Value::Null);
        }
        Some(match value {
            Value::Object(map) => {
                if depth > 0 && noise.chance(3) {
                    return Some(json!({}));
                }
                Value::Object(
                    map.iter()
                        .filter_map(|(k, v)| self.mutate(v, noise, depth + 1).map(|v| (k.clone(), v)))
                        .collect(),
                )
            }
            Value::Array(items) => match noise.below(8) {
                0 => json!([]),
                1 => Value::Array(items.iter().chain(items.iter()).cloned().collect()),
                _ => Value::Array(items.iter().filter_map(|v| self.mutate(v, noise, depth + 1)).collect()),
            },
            Value::Number(n) => match noise.chance(3) {
                true => self.string(noise, "7"),
                false => self.number(noise, n),
            },
            Value::String(s) => match noise.chance(3) {
                true => json!(noise.below(100)),
                false => self.string(noise, s),
            },
            Value::Bool(b) => match noise.chance(3) {
                true => json!("true"),
                false => json!(noise.chance(50) ^ b),
            },
            Value::Null => match noise.below(3) {
                0 => json!(1),
                1 => self.string(noise, "x"),
                _ => Value::Null,
            },
        })
    }

    fn rows(fixture: &Fixture, count: usize, seed: u64) -> Vec<Value> {
        let wide = Wide::of(fixture);
        let mut noise = Noise::new(seed);
        (0..count)
            .map(|_| {
                let base = &fixture.inputs[noise.below(fixture.inputs.len())];
                wide.mutate(base, &mut noise, 0).unwrap_or(json!({}))
            })
            .collect()
    }

    fn seed(name: &str) -> u64 {
        name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
    }
}

#[cfg(feature = "driver")]
#[tokio::test]
#[ignore]
async fn wide_inputs_match_the_walker() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let rows: usize = std::env::var("WIDE_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let filter = std::env::var("WIDE_FILE").ok();
    let mut report: Vec<(String, usize, usize, usize)> = Vec::new();
    let mut samples: Vec<String> = Vec::new();
    for fixture in Fixture::load_all() {
        if NETWORK_BOUND.contains(&fixture.name.as_str())
            || TIME_BOUND.contains(&fixture.name.as_str())
            || filter.as_ref().is_some_and(|f| !fixture.name.contains(f.as_str()))
        {
            continue;
        }
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        if !matches!(compiled.compiled_verdict(), Some(Ok(()))) {
            continue;
        }
        let walker = compiled.interpreted();
        let inputs = Wide::rows(&fixture, rows, Wide::seed(&fixture.name));
        let contexts: Vec<Variable> = inputs.iter().map(|v| Variable::from(v.clone())).collect();
        let batch = compiled.evaluate_batch(&contexts, EvaluationOptions::default()).await;
        let mut wants = Vec::with_capacity(inputs.len());
        let mut batched = 0usize;
        for (i, input) in inputs.iter().enumerate() {
            let want = Outcome::of(walker.evaluate(Variable::from(input.clone())).await);
            let got = batch.get(i).map(|r| match r {
                Ok(r) => Ok(serde_json::to_string(&r.result).unwrap_or_default()),
                Err(e) => Err(serde_json::to_value(&**e).map(|v| v.to_string()).unwrap_or_else(|_| e.to_string())),
            });
            if got.as_ref() != Some(&want) {
                batched += 1;
                if samples.len() < 12 {
                    samples.push(format!("{} batch {input}\n  walker   {want:?}\n  compiled {got:?}", fixture.name));
                }
            }
            wants.push(want);
        }
        let mut columnar = 0usize;
        let mut compared = 0usize;
        for built in Built::new(&inputs).into_iter().chain(Built::lists(&inputs)).chain(Built::nested(&inputs)) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            let output = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
            for row in 0..built.rows {
                let input = columns.row(row);
                let want = match walker.evaluate(input.clone()).await {
                    Ok(r) => Ok(Built::normalized(r.result.to_value())),
                    Err(e) => Err(serde_json::to_value(&*e).map(|v| v.to_string()).unwrap_or_default()),
                };
                let got = match &output.errors[row] {
                    Some(e) => Err(serde_json::to_value(&**e).map(|v| v.to_string()).unwrap_or_default()),
                    None => Ok(Built::normalized(output.row(row).to_value())),
                };
                compared += 1;
                if got != want {
                    columnar += 1;
                    if samples.len() < 24 {
                        samples.push(format!("{} columnar {}\n  walker   {want:?}\n  columnar {got:?}", fixture.name, input.to_value()));
                    }
                }
            }
        }
        if std::env::var("WIDE_SHOW").is_ok() {
            let distinct: std::collections::BTreeSet<String> = inputs.iter().map(|v| v.to_string()).collect();
            let errors = wants.iter().filter(|w| w.is_err()).count();
            println!("{:44} distinct {:>4} errors {:>4} columnar-compared {:>4} first {}", fixture.name, distinct.len(), errors, compared, inputs[0]);
        }
        report.push((fixture.name.clone(), batched, columnar, compared));
    }
    let failing: Vec<&(String, usize, usize, usize)> = report.iter().filter(|r| r.1 > 0 || r.2 > 0).collect();
    for (name, batched, columnar, compared) in &failing {
        println!("{name:44} row {batched:>4}/{rows} columnar {columnar:>4}/{compared}");
    }
    println!("wide inputs: {} fixtures, {} with mismatches", report.len(), failing.len());
    for sample in &samples {
        println!("{sample}");
    }
    assert!(failing.is_empty() || std::env::var("WIDE_REPORT").is_ok(), "{} fixtures mismatch on wide inputs", failing.len());
}

#[tokio::test]
#[ignore]
async fn plan_census() {
    let settled: Vec<String> = std::env::var("CENSUS_SETTLED")
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|text| text.lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut reasons: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for fixture in Fixture::load_all() {
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        let variants = match std::env::var("BENCH_VALID").is_ok() && !fixture.inputs.is_empty() {
            true => fixture.inputs.clone(),
            false => fixture.typed_variants(),
        };
        let variants: Vec<Value> = match std::env::var("CENSUS_ROWS").ok().and_then(|v| v.parse::<usize>().ok()) {
            Some(rows) if !variants.is_empty() => (0..rows).map(|i| variants[i % variants.len()].clone()).collect(),
            _ => variants,
        };
        let Some(built) = Built::new(&variants) else {
            reasons.entry("no columns".into()).or_default().push(fixture.name.clone());
            continue;
        };
        let columns = built.columns();
        let _ = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
        let _ = Decision::census();
        let start = std::time::Instant::now();
        let _ = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
        println!("CENSUS\t{}\ttotal\t1\t{}\t{}", fixture.name, built.rows, start.elapsed().as_nanos());
        for (key, calls, rows, ns) in Decision::census() {
            println!("CENSUS\t{}\t{key}\t{calls}\t{rows}\t{ns}", fixture.name);
        }
        let key = match compiled.plan_verdict(&columns) {
            None => "not compiled".to_string(),
            Some(Ok(())) => "planned".to_string(),
            Some(Err(reason)) => reason,
        };
        let mark = if settled.contains(&fixture.name) { "*" } else { "" };
        reasons.entry(key).or_default().push(format!("{}{mark}", fixture.name));
    }
    for (reason, names) in &reasons {
        println!("{:>3} {reason}: {}", names.len(), names.join(" "));
    }
}

#[cfg(feature = "driver")]
struct Chain;

#[cfg(feature = "driver")]
impl Chain {
    const FULLY_TYPED: &[&str] = &[
        "affiliate-commission-calculator.json",
        "airline-upgrade-eligibility.json",
        "auto-insurance-premium-calculator.json",
        "delivery-route-optimizer.json",
        "dynamic-fx-rate-pricing-system.json",
        "environment-compliance-assessment.json",
    ];

    fn typed() -> bool {
        std::env::var("ZEN_TYPED").map_or(true, |v| v.trim() != "0")
    }

    fn graph(nodes: Vec<Value>) -> GraphContent {
        let mut all = vec![json!({"id": "input", "name": "request", "type": "inputNode", "content": {}})];
        all.extend(nodes);
        all.push(json!({"id": "output", "name": "response", "type": "outputNode", "content": {}}));
        let edges: Vec<Value> = all
            .windows(2)
            .enumerate()
            .map(|(at, pair)| json!({"id": format!("e{at}"), "sourceId": pair[0]["id"], "targetId": pair[1]["id"], "type": "edge"}))
            .collect();
        serde_json::from_value(json!({"nodes": all, "edges": edges})).expect("graph")
    }

    fn expression(id: &str, pairs: &[(&str, &str)]) -> Value {
        let expressions: Vec<Value> = pairs.iter().enumerate().map(|(at, (key, value))| json!({"id": format!("{id}-{at}"), "key": key, "value": value})).collect();
        json!({"id": id, "name": id, "type": "expressionNode", "content": {"expressions": expressions, "passThrough": true, "inputField": null, "outputPath": null, "executionMode": "single"}})
    }

    fn table(id: &str, inputs: &[&str], outputs: &[&str], rules: &[&[&str]]) -> Value {
        let inputs_json: Vec<Value> = inputs.iter().enumerate().map(|(at, field)| json!({"id": format!("i{at}"), "name": field, "field": field})).collect();
        let outputs_json: Vec<Value> = outputs.iter().enumerate().map(|(at, field)| json!({"id": format!("o{at}"), "name": field, "field": field})).collect();
        let rules_json: Vec<Value> = rules
            .iter()
            .enumerate()
            .map(|(at, cells)| {
                let mut rule = serde_json::Map::new();
                rule.insert("_id".into(), json!(format!("r{at}")));
                for (column, cell) in cells.iter().enumerate() {
                    let key = match column < inputs.len() {
                        true => format!("i{column}"),
                        false => format!("o{}", column - inputs.len()),
                    };
                    rule.insert(key, json!(cell));
                }
                Value::Object(rule)
            })
            .collect();
        json!({"id": id, "name": id, "type": "decisionTableNode", "content": {"hitPolicy": "first", "inputs": inputs_json, "outputs": outputs_json, "rules": rules_json, "passThrough": true, "inputField": null, "outputPath": null, "executionMode": "single"}})
    }

    async fn checked(content: GraphContent, rows: &[Value], failures: &mut Vec<String>) -> (usize, usize) {
        let mut compiled = Decision::from(Arc::new(content));
        compiled.compile();
        assert!(matches!(compiled.compiled_verdict(), Some(Ok(()))), "verdict {:?}", compiled.compiled_verdict());
        let walker = compiled.interpreted();
        let built = Built::new(rows).expect("columns");
        let columns = built.columns();
        let output = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
        for row in 0..built.rows {
            let input = columns.row(row);
            let want = match walker.evaluate(input.clone()).await {
                Ok(r) => Ok(Built::normalized(r.result.to_value()).to_string()),
                Err(e) => Err(serde_json::to_value(&*e).map(|v| v.to_string()).unwrap_or_default()),
            };
            let got = match &output.errors[row] {
                Some(e) => Err(serde_json::to_value(&**e).map(|v| v.to_string()).unwrap_or_default()),
                None => Ok(Built::normalized(output.row(row).to_value()).to_string()),
            };
            if got != want {
                failures.push(format!("row {row} input {}\n  walker   {want:?}\n  columnar {got:?}", input.to_value()));
            }
        }
        (output.typed, output.laned)
    }

    fn rows(count: usize, seed: u64) -> Vec<Value> {
        let mut noise = Noise::new(seed);
        let tiers = ["gold", "silver", "bronze", "platinum"];
        (0..count)
            .map(|_| {
                let income = noise.below(9000) as i64 - 500;
                let cents = noise.below(100);
                json!({"tier": tiers[noise.below(tiers.len())], "income": income, "rate": format!("{}.{cents:02}", noise.below(5)).parse::<serde_json::Number>().map(Value::Number).unwrap_or(Value::Null), "age": noise.below(90)})
            })
            .collect()
    }

    fn nodes(middle: Option<Value>) -> Vec<Value> {
        let mut nodes = vec![
            Self::table(
                "band",
                &["tier", "income"],
                &["band", "factor"],
                &[&["'gold'", "> 5000", "'high'", "1.25"], &["'gold', 'platinum'", "", "'mid'", "1.10"], &["", "[0..1000]", "'low'", "0.5"], &["", "", "'none'", "1"]],
            ),
            Self::expression("score", &[("net", "income - 100"), ("scaled", "$.net * factor"), ("adult", "age >= 18"), ("third", "$.net / 3")]),
        ];
        nodes.extend(middle);
        nodes.push(Self::expression("final", &[("total", "scaled + rate"), ("share", "total / 4"), ("rich", "band == 'high' and adult"), ("large", "third > 100 or scaled < 0")]));
        nodes
    }
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn graph_typed_falls_back_per_unit() {
    let mut failures = Vec::new();
    let unsupported = Chain::expression("label", &[("label", "split(tier, 'l')")]);
    let (typed, laned) = Chain::checked(Chain::graph(Chain::nodes(Some(unsupported))), &Chain::rows(300, 3), &mut failures).await;
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    if Chain::typed() {
        assert!(typed > 0 && laned > 0, "units typed {typed}, laned {laned}");
        assert_eq!(typed + laned, 4, "units typed {typed}, laned {laned}");
    }
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn graph_typed_reads_dynamic_slots() {
    let mut rows = Chain::rows(240, 5);
    let odd = [
        json!({"tier": "gold", "income": "lots", "rate": 1, "age": 30}),
        json!({"tier": "gold", "income": {"value": 5}, "rate": 0.5, "age": 70}),
        json!({"tier": "silver", "income": 79228162514264337593543950335u128, "rate": 0, "age": 40}),
        json!({"tier": "gold", "income": true, "rate": 0.25, "age": 60}),
        json!({"tier": "bronze", "income": -0.0, "rate": 1.50, "age": 18}),
        json!({"tier": null, "income": null, "rate": null, "age": null}),
        json!({"tier": "gold", "income": 6000.50, "rate": 2, "age": 55}),
    ];
    for (at, row) in odd.into_iter().enumerate() {
        rows.insert(at * 31 + 3, row);
    }
    let nodes = vec![
        Chain::expression("pick", &[("pick", "age > 20 ? income : tier")]),
        Chain::expression("half", &[("half", "income / 2")]),
        Chain::table("level", &["pick", "half"], &["level"], &[&["> 3000", "", "'top'"], &["", "> 100", "'some'"], &["", "", "'none'"]]),
        Chain::expression("final", &[("bonus", "half * rate"), ("big", "level == 'top' or $.bonus > 50")]),
    ];
    let mut failures = Vec::new();
    let (typed, laned) = Chain::checked(Chain::graph(nodes), &rows, &mut failures).await;
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    if Chain::typed() {
        assert!(typed > 0, "units typed {typed}, laned {laned}");
    }
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn graph_typed_lists_and_closures() {
    let mut noise = Noise::new(17);
    let tags = ["a", "vip", "b"];
    let mut rows: Vec<Value> = (0..300)
        .map(|_| {
            let xs: Vec<Value> = (0..noise.below(6)).map(|_| json!(format!("{}.{}", noise.below(40) as i64 - 10, noise.below(10)).parse::<serde_json::Number>().map(Value::Number).unwrap_or(Value::Null))).collect();
            let tg: Vec<Value> = (0..noise.below(3)).map(|_| json!(tags[noise.below(3)])).collect();
            let items: Vec<Value> = (0..noise.below(5))
                .map(|_| {
                    let cells: Vec<Value> = (0..noise.below(3)).map(|_| json!({"p": noise.below(50)})).collect();
                    json!({"qty": noise.below(8), "price": noise.below(500), "ok": noise.below(3) != 0, "cells": cells})
                })
                .collect();
            json!({"xs": xs, "tags": tg, "items": items, "limit": noise.below(20), "n": noise.below(12) as i64 - 3})
        })
        .collect();
    let odd = [
        json!({"xs": null, "tags": ["vip"], "items": null, "limit": 3, "n": 1}),
        json!({"xs": [1, null, 3], "tags": [], "items": [], "limit": 2, "n": 3}),
        json!({"xs": ["a", 2], "tags": ["b", 7], "items": [{"qty": 1, "price": 2, "ok": null, "cells": []}], "limit": 1, "n": 2}),
        json!({"xs": [9223372036854775807i64, 9223372036854775807i64], "tags": ["a"], "items": [{"qty": 9223372036854775807i64, "price": 3, "ok": true, "cells": [{"p": 9223372036854775807i64}, {"p": 1}]}], "limit": 0, "n": 0}),
        json!({"xs": [0.00, -0.0, 1.50], "tags": ["vip", "vip"], "items": [{"qty": 2, "price": 1.25, "ok": true, "cells": null}], "limit": null, "n": null}),
        json!({"limit": 5, "n": 5}),
        json!({"xs": [], "tags": null, "items": [{"qty": 3}, {"price": 4, "ok": false, "cells": [{"q": 1}]}], "limit": 1, "n": -1}),
        json!({"xs": [79228162514264337593543950335u128], "tags": ["a"], "items": [{"qty": 1, "price": 0.000000001, "ok": true, "cells": [{"p": 0.5}]}], "limit": 1, "n": 1}),
    ];
    for (at, row) in odd.into_iter().enumerate() {
        rows.insert(at * 29 + 5, row);
    }
    let nodes = vec![
        Chain::expression(
            "lists",
            &[
                ("total", "sum(xs)"),
                ("pos", "filter(xs, # > limit)"),
                ("doubled", "map(xs, # * 2)"),
                ("has", "n in xs"),
                ("vip", "'vip' in tags"),
                ("qtys", "map(filter(items, #.ok), #.qty)"),
                ("ct", "count(filter(items, #.ok), #.price > limit)"),
                ("cells", "sum(map(items, sum(map(#.cells, #.p))))"),
                ("safe", "sum(map(items ?? [], #.qty))"),
                ("copy", "xs ?? []"),
                ("hit", "contains(xs, 2)"),
            ],
        ),
        Chain::expression("later", &[("big", "len(filter(doubled, # > 10))"), ("sumd", "sum(doubled)"), ("anyq", "some(qtys, # > 3)"), ("lo", "min(map(items, #.price))")]),
    ];
    let mut failures = Vec::new();
    let (typed, laned) = Chain::checked(Chain::graph(nodes), &rows, &mut failures).await;
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    if Chain::typed() {
        assert!(typed > 0, "units typed {typed}, laned {laned}");
    }
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn graph_typed_strings() {
    let mut noise = Noise::new(29);
    let names = ["Anna", "bob", "  Cara  ", "Émile", "straße", "", "İstanbul", "日本", "Bart"];
    let cities = ["Berlin", "bonn", "Anna", " x ", ""];
    let tiers = ["gold", "silver", "Gold"];
    let mut rows: Vec<Value> = (0..300)
        .map(|_| {
            let items: Vec<Value> = (0..noise.below(4)).map(|_| json!({"name": names[noise.below(names.len())], "qty": noise.below(9), "tag": tiers[noise.below(3)]})).collect();
            let tags: Vec<Value> = (0..noise.below(3)).map(|_| json!(cities[noise.below(cities.len())])).collect();
            let amount = format!("{}.{}", noise.below(4000) as i64 - 2000, noise.below(100)).parse::<serde_json::Number>().map(Value::Number).unwrap_or(Value::Null);
            json!({"name": names[noise.below(names.len())], "city": cities[noise.below(cities.len())], "tier": tiers[noise.below(3)], "amount": amount, "qty": noise.below(10), "flag": noise.below(2) == 0, "items": items, "tags": tags})
        })
        .collect();
    let long = "ab".repeat(600);
    let odd = [
        json!({"name": null, "city": "Berlin", "tier": null, "amount": null, "qty": 3, "flag": null, "items": null, "tags": null}),
        json!({"name": long, "city": long, "tier": "gold", "amount": 79228162514264337593543950335u128, "qty": 9223372036854775807i64, "flag": false, "items": [], "tags": []}),
        json!({"name": "ß", "city": "ǅ", "tier": "", "amount": -0.0, "qty": 0, "flag": true, "items": [{"name": "ß", "qty": 1, "tag": null}], "tags": ["ǅ"]}),
        json!({"name": "\u{1F600} x", "city": "\u{2003}a\u{2003}", "tier": "silver", "amount": 0.000000001, "qty": 6, "flag": false, "items": [{"qty": 2}], "tags": ["\u{2003}a\u{2003}"]}),
        json!({"qty": 5}),
        json!({"name": "Anna", "city": "Anna", "tier": "gold", "amount": 1.50, "qty": 8, "flag": true, "items": [{"name": "gold", "qty": 9223372036854775807i64, "tag": "gold"}], "tags": ["Anna", "x"]}),
        json!({"name": "", "city": "", "tier": "silver", "amount": 0.00, "qty": 2, "flag": true, "items": [{"name": "", "qty": 0}], "tags": [""]}),
    ];
    for (at, row) in odd.into_iter().enumerate() {
        rows.insert(at * 31 + 2, row);
    }
    let nodes = vec![
        Chain::expression(
            "texts",
            &[
                ("s", "string(amount)"),
                ("tpl", "`${name}-${qty}:${amount} ${flag}`"),
                ("cat", "name + '/' + city"),
                ("up", "upper(name)"),
                ("lo", "lower(city)"),
                ("tr", "trim(name)"),
                ("has", "contains(lower(name), 'an')"),
                ("tierz", "tier ?? 'none'"),
                ("pick", "qty > 4 ? name : 'nobody'"),
                ("same", "name == city"),
                ("pass", "city"),
                ("rx", "matches(name, '^[A-Z]')"),
                ("names", "map(items, #.name)"),
                ("upn", "map(items, upper(#.name) + string(#.qty))"),
                ("cnt", "count(items, #.name == tier)"),
                ("intag", "city in tags"),
                ("big", "string(amount * 1000000000000)"),
                ("mixeq", "name == qty"),
            ],
        ),
        Chain::table("level", &["qty", "tierz"], &["level", "who"], &[&["> 7", "'gold'", "'top'", "name"], &["> 3", "", "'mid'", "city"], &["", "", "'low'", "'nobody'"]]),
        Chain::expression("later", &[("again", "upper(tierz) + '!'"), ("g", "tierz in ['gold', 'none']"), ("l", "len(cat)"), ("w", "level + ':' + (who ?? '-')")]),
    ];
    let mut failures = Vec::new();
    let (typed, laned) = Chain::checked(Chain::graph(nodes.clone()), &rows, &mut failures).await;
    eprintln!("strings typed {typed} laned {laned}");
    if Chain::typed() {
        assert!(typed > laned, "units typed {typed}, laned {laned}");
    }
    rows.insert(12, json!({"name": "q", "city": "q", "tier": "gold", "amount": 1, "qty": 2, "flag": true, "items": [{"name": null, "qty": 2}], "tags": [null]}));
    rows.insert(9, json!({"name": 7, "city": true, "tier": 1.5, "amount": "x", "qty": 1, "flag": true, "items": [{"name": 3, "qty": "a"}], "tags": [1]}));
    Chain::checked(Chain::graph(nodes), &rows, &mut failures).await;
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn graph_typed_dates() {
    Reference::prepare(Outcome::CLOCK);
    let mut noise = Noise::new(31);
    let days = [
        "2024-01-15", "2024-02-29", "2023-12-31 23:59:59", "2024-03-31T01:30:00Z", "2025-03-30 02:30:00", "2024-10-27 02:30:00", "1900-01-01", "9999-12-31",
        "2024-06-15T12:00:00+05:30", "2024-06-15T12:00:00.123456789Z", "20240115", "2024/01/15", "2024", "2024-07", "2025-01-31", "2025-08-19",
    ];
    let bad = ["nope", "", "2024-02-30", "2024-13-01"];
    let mut day = |noise: &mut Noise| match noise.below(12) {
        0 => json!(bad[noise.below(bad.len())]),
        1 => Value::Null,
        _ => json!(days[noise.below(days.len())]),
    };
    let mut rows: Vec<Value> = (0..300)
        .map(|_| {
            let items: Vec<Value> = (0..noise.below(4)).map(|_| json!({"on": day(&mut noise), "gb": noise.below(9)})).collect();
            json!({"at": day(&mut noise), "other": day(&mut noise), "ms": noise.below(2_000_000_000) as i64 * 1000 - 1_000_000_000_000, "n": noise.below(7), "items": items})
        })
        .collect();
    let odd = [
        json!({"at": "0001-01-01", "other": "9999-12-31 23:59:59", "ms": 9223372036854775807i64, "n": 0, "items": []}),
        json!({"at": null, "other": null, "ms": null, "n": 3, "items": [{"on": null, "gb": 1}]}),
        json!({"at": "2024-02-29", "other": "2024-02-29T00:00:00Z", "ms": -0.0, "n": 6, "items": [{"on": "2024-02-29", "gb": 2}]}),
        json!({"at": "Europe/Berlin", "other": "UTC", "ms": 1.5, "n": 1, "items": null}),
    ];
    for (at, row) in odd.into_iter().enumerate() {
        rows.insert(at * 41 + 3, row);
    }
    let nodes = vec![
        Chain::expression(
            "dates",
            &[
                ("p", "d(at)"),
                ("ps", "string(d(at))"),
                ("m", "d(ms)"),
                ("y", "d(at).year()"),
                ("add", "d(at).add(1, 'month')"),
                ("addn", "d(at).add(n, 'day')"),
                ("som", "d(at).startOf('month')"),
                ("fmt", "d(at).format('%Y-%m-%d %H:%M')"),
                ("diff", "d(at).diff(other, 'day')"),
                ("bef", "d(at).isBefore(d('2024-03-01'))"),
                ("same", "d(at).isSame(d(other), 'month')"),
                ("lt", "d(at) < d(other)"),
                ("eqs", "d(at) == other"),
                ("inl", "d(at) in [d('2024-01-15'), '2024-02-29']"),
                ("rng", "d(at) in [d('2024-01-01')..d('2024-12-31')]"),
                ("tpl", "`at ${d(at)}`"),
                ("now", "d().diff(d(at), 'year')"),
                ("cnt", "count(items, d(#.on).isAfter(d(at)))"),
                ("dts", "map(items, d(#.on).add(1, 'd'))"),
                ("mm", "max([d(at), d(other)])"),
                ("leg", "date('2024-01-01') + year('2024-06-30')"),
            ],
        ),
        Chain::table("band", &["at"], &["band", "since"], &[&["< d('2000-01-01')", "'old'", "d('1999-12-31')"], &["d($).isValid() == false", "'bad'", "null"], &["", "'mid'", "d(at)"]]),
        Chain::expression("later", &[("again", "p.add(1, 'd')"), ("s", "string(add)"), ("old", "since == null or since < d()")]),
    ];
    let mut failures = Vec::new();
    let (typed, laned) = Chain::checked(Chain::graph(nodes.clone()), &rows, &mut failures).await;
    eprintln!("dates typed {typed} laned {laned}");
    if Chain::typed() {
        assert!(typed > laned, "units typed {typed}, laned {laned}");
    }
    let faulty = vec![Chain::expression("faulty", &[("lts", "d(at) < other"), ("cat", "'D:' + d(at)"), ("mx", "max(map(items, d(#.on)))"), ("legd", "dateString(at)")])];
    Chain::checked(Chain::graph(faulty), &rows, &mut failures).await;
    rows.insert(12, json!({"at": 5, "other": true, "ms": "x", "n": 1, "items": [{"on": 3}]}));
    Chain::checked(Chain::graph(nodes), &rows, &mut failures).await;
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn graph_typed_extremes() {
    let mut rows = Chain::rows(200, 9);
    let odd = [
        json!({"tier": "gold", "income": 9223372036854775807i64, "rate": 1, "age": 30}),
        json!({"tier": "gold", "income": -9223372036854775807i64, "rate": 0.5, "age": 30}),
        json!({"tier": "silver", "income": 79228162514264337593543950335u128, "rate": 0, "age": 40}),
        json!({"tier": "gold", "income": 12.5, "rate": 0.000000001, "age": 18}),
        json!({"tier": "gold", "income": 0.00, "rate": 0.00, "age": 17}),
        json!({"tier": "bronze", "income": -0.0, "rate": 1.50, "age": 18}),
        json!({"tier": null, "income": null, "rate": null, "age": null}),
        json!({"tier": "platinum", "income": 1000, "rate": 3.333, "age": 0}),
        json!({"tier": "", "income": 1000.000, "rate": 2.10, "age": 65}),
        json!({"income": 7, "age": 5}),
        json!({"tier": "gold", "income": 5000.01, "rate": 0.1, "age": 90}),
        json!({"tier": "gold", "income": 100, "rate": 0.3333333333333333333333333333, "age": 20}),
    ];
    for (at, row) in odd.into_iter().enumerate() {
        rows.insert(at * 13 + 4, row);
    }
    let mut failures = Vec::new();
    let (typed, laned) = Chain::checked(Chain::graph(Chain::nodes(None)), &rows, &mut failures).await;
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    if Chain::typed() {
        assert!(typed > 0, "units typed {typed}, laned {laned}");
        assert_eq!(typed + laned, 3, "units typed {typed}, laned {laned}");
    }
}

#[cfg(feature = "driver")]
#[tokio::test]
async fn graph_fixtures_are_fully_typed() {
    Reference::prepare(Outcome::CLOCK);
    let mut failures = Vec::new();
    let fixtures: Vec<Fixture> = Fixture::load_all().into_iter().filter(|f| Chain::FULLY_TYPED.contains(&f.name.as_str())).collect();
    assert_eq!(fixtures.len(), Chain::FULLY_TYPED.len(), "fixtures found");
    for fixture in fixtures {
        let variants = fixture.typed_variants();
        let rows: Vec<Value> = (0..256).map(|at| variants[at % variants.len()].clone()).collect();
        let (typed, laned) = Chain::checked(fixture.content.clone(), &rows, &mut failures).await;
        if Chain::typed() {
            assert!(typed > 0 && laned == 0, "{}: units typed {typed}, laned {laned}", fixture.name);
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}

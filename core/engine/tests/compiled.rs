mod support;

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use support::columns::Built;
use support::test_data_root;
use zen_engine::model::{DecisionContent, GraphContent};
use zen_engine::{Decision, EvaluationOptions};
use zen_expression::Variable;

struct Fixture {
    name: String,
    content: GraphContent,
    inputs: Vec<Value>,
}

impl Fixture {
    fn load_all() -> Vec<Fixture> {
        let root = PathBuf::from(test_data_root());
        let mut paths: Vec<PathBuf> = fs::read_dir(root.join("graphs"))
            .into_iter()
            .flatten()
            .chain(fs::read_dir(&root).into_iter().flatten())
            .chain(fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/compiled")).into_iter().flatten())
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        paths.iter().filter_map(|p| Self::load(p)).collect()
    }

    fn load(path: &Path) -> Option<Fixture> {
        let text = fs::read_to_string(path).ok()?;
        let raw: Value = serde_json::from_str(&text).ok()?;
        let content: DecisionContent = serde_json::from_value(raw.clone()).ok()?;
        let DecisionContent::Graph(graph) = content else {
            return None;
        };
        let mut inputs: Vec<Value> = raw
            .get("tests")
            .and_then(Value::as_array)
            .map(|tests| tests.iter().filter_map(|t| t.get("input").cloned()).collect())
            .unwrap_or_default();
        if inputs.is_empty() {
            inputs.push(json!({}));
        }
        Some(Fixture {
            name: path.file_name()?.to_string_lossy().to_string(),
            content: (*graph).clone(),
            inputs,
        })
    }

    fn typed_variants(&self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for base in &self.inputs {
            out.push(base.clone());
            let Some(object) = base.as_object() else {
                continue;
            };
            for (key, value) in object {
                let mut with = |replacement: Option<Value>| {
                    let mut copy = object.clone();
                    match replacement {
                        Some(v) => copy.insert(key.clone(), v),
                        None => copy.remove(key),
                    };
                    out.push(Value::Object(copy));
                };
                with(None);
                match value {
                    Value::Number(n) => {
                        for delta in ["1", "-1", "0", "1000000", "0.5"] {
                            let shifted: Option<rust_decimal::Decimal> = n
                                .to_string()
                                .parse::<rust_decimal::Decimal>()
                                .ok()
                                .zip(delta.parse::<rust_decimal::Decimal>().ok())
                                .and_then(|(a, b)| a.checked_add(b));
                            if let Some(v) = shifted.and_then(|d| serde_json::from_str(&d.to_string()).ok()) {
                                with(Some(v));
                            }
                        }
                    }
                    Value::String(s) => {
                        with(Some(json!("")));
                        with(Some(json!(format!("{s}x"))));
                    }
                    Value::Bool(b) => with(Some(json!(!b))),
                    _ => {}
                }
            }
        }
        out.truncate(400);
        out
    }

    fn variants(&self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for base in &self.inputs {
            out.push(base.clone());
            let Some(object) = base.as_object() else {
                continue;
            };
            for (key, value) in object {
                let mut with = |replacement: Option<Value>| {
                    let mut copy = object.clone();
                    match replacement {
                        Some(v) => {
                            copy.insert(key.clone(), v);
                        }
                        None => {
                            copy.remove(key);
                        }
                    }
                    out.push(Value::Object(copy));
                };
                with(None);
                with(Some(Value::Null));
                match value {
                    Value::Number(n) => {
                        let text = n.to_string();
                        for delta in ["1", "-1", "0", "1000000", "0.5"] {
                            let shifted: Option<rust_decimal::Decimal> = text
                                .parse::<rust_decimal::Decimal>()
                                .ok()
                                .zip(delta.parse::<rust_decimal::Decimal>().ok())
                                .and_then(|(a, b)| a.checked_add(b));
                            if let Some(v) = shifted.and_then(|d| serde_json::from_str(&d.to_string()).ok()) {
                                with(Some(v));
                            }
                        }
                        with(Some(json!("text")));
                    }
                    Value::String(s) => {
                        with(Some(json!("")));
                        with(Some(json!(format!("{s}x"))));
                        with(Some(json!(42)));
                    }
                    Value::Bool(b) => with(Some(json!(!b))),
                    Value::Array(items) => {
                        with(Some(json!([])));
                        let mut doubled = items.clone();
                        doubled.extend(items.iter().cloned());
                        with(Some(Value::Array(doubled)));
                    }
                    Value::Object(_) => with(Some(json!({}))),
                    Value::Null => with(Some(json!(1))),
                }
            }
        }
        out.truncate(400);
        out
    }
}

const NETWORK_BOUND: &[&str] = &["http-function.json"];
const TIME_BOUND: &[&str] = &["infinite-function.json", "sleep-function.json"];

struct Outcome;

impl Outcome {
    fn of(result: Result<zen_engine::DecisionGraphResponse, Box<zen_engine::EvaluationError>>) -> Result<String, String> {
        result
            .map(|r| serde_json::to_string(&r.result).unwrap_or_default())
            .map_err(|e| serde_json::to_value(&*e).map(|v| v.to_string()).unwrap_or_else(|_| e.to_string()))
    }
}

#[tokio::test]
async fn compiled_graphs_match_the_walker() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let mut census: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0usize;
    for fixture in Fixture::load_all() {
        let walker = Decision::from(Arc::new(fixture.content.clone()));
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
            let want = Outcome::of(walker.evaluate(Variable::from(input.clone())).await);
            let single = Outcome::of(compiled.evaluate(Variable::from(input.clone())).await);
            let batched = batch.get(i).map(|r| match r {
                Ok(r) => Ok(serde_json::to_string(&r.result).unwrap_or_default()),
                Err(e) => Err(serde_json::to_value(&**e).map(|v| v.to_string()).unwrap_or_else(|_| e.to_string())),
            });
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
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

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
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
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
        let walker = compiled.interpreted();
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
                let want = walker.evaluate(input.clone()).await;
                let want = match want {
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
    }
    println!("compared {compared} rows, {skipped} fixtures skipped");
    let mut kinds: Vec<&str> = failures.iter().filter_map(|f| f.split_whitespace().next()).collect();
    kinds.dedup();
    assert!(failures.is_empty(), "{} mismatches in {kinds:?}:\n{}", failures.len(), failures.iter().take(15).cloned().collect::<Vec<_>>().join("\n"));
}

#[tokio::test]
async fn struct_inputs_match_the_walker() {
    use zen_expression::lane::{Column, Columns, Values};
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
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

#[tokio::test]
#[ignore]
async fn columnar_graph_throughput() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let filter = std::env::var("BENCH_FILE").ok();
    let mut ratios: Vec<(f64, String)> = Vec::new();
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
            let _ = compiled.evaluate_columns(&columns, options).await;
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

struct Noise(u64);

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

struct Wide {
    numbers: Vec<rust_decimal::Decimal>,
    strings: Vec<String>,
}

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
        let Some(built) = Built::new(&variants) else {
            reasons.entry("no columns".into()).or_default().push(fixture.name.clone());
            continue;
        };
        let columns = built.columns();
        let _ = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
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

struct Chain;

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

#[tokio::test]
async fn graph_typed_falls_back_per_unit() {
    let mut failures = Vec::new();
    let unsupported = Chain::expression("label", &[("label", "string(income) + ' ' + tier")]);
    let (typed, laned) = Chain::checked(Chain::graph(Chain::nodes(Some(unsupported))), &Chain::rows(300, 3), &mut failures).await;
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    if Chain::typed() {
        assert!(typed > 0 && laned > 0, "units typed {typed}, laned {laned}");
        assert_eq!(typed + laned, 4, "units typed {typed}, laned {laned}");
    }
}

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

#[tokio::test]
async fn graph_fixtures_are_fully_typed() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
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

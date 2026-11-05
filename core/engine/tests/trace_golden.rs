mod conformance;
mod support;

use conformance::run::{GraphRun, Guard, PolicyRun};
use conformance::{Dialect, Spec};
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::Ordering;
use zen_engine::trace::TraceFormat;
use zen_engine::{DecisionGraphResponse, EvaluationError, EvaluationOptions, ZEN_CONFIG};

fn options(format: TraceFormat) -> EvaluationOptions {
    EvaluationOptions {
        trace: format,
        max_depth: 10,
        ..Default::default()
    }
}

fn canonical(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = serde_json::Map::new();
            for (k, x) in entries {
                let x = match k.as_str() {
                    "performance" | "duration" | "msSinceRun" => Value::Null,
                    _ => canonical(x),
                };
                out.insert(k, x);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
        Value::String(text) => Value::String(addresses(&text)),
        other => other,
    }
}

fn addresses(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("0x") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let digits = tail
            .find(|c: char| !matches!(c, '0'..='9' | 'a'..='f'))
            .unwrap_or(tail.len());
        match digits {
            0 => out.push_str("0x"),
            _ => out.push_str("0xADDR"),
        }
        rest = &tail[digits..];
    }
    out.push_str(rest);
    out
}

fn graph(result: &Result<DecisionGraphResponse, Box<EvaluationError>>) -> Value {
    canonical(match result {
        Ok(r) => json!({"ok": serde_json::to_value(r).unwrap()}),
        Err(e) => json!({"err": serde_json::to_value(&**e).unwrap()}),
    })
}

fn serial(result: Result<Value, Value>) -> Value {
    canonical(match result {
        Ok(v) => json!({"ok": v}),
        Err(v) => json!({"err": v}),
    })
}

fn policy(
    result: Result<zen_engine::policy::EvaluationResult, zen_engine::policy::EvaluationError>,
) -> Value {
    canonical(match result {
        Ok(r) => json!({"ok": serde_json::to_value(&r).unwrap()}),
        Err(e) => json!({"err": serde_json::to_value(EvaluationError::Policy(e)).unwrap()}),
    })
}

fn engines(compiled: bool) -> &'static str {
    ZEN_CONFIG.compiled.store(compiled, Ordering::Relaxed);
    match compiled {
        true => "compiled",
        false => "interpreted",
    }
}

pub struct Run<'a> {
    pub kind: &'static str,
    pub suite: &'a str,
    pub case: usize,
    pub variant: String,
    pub outcome: Value,
    pub request: &'a zen_expression::Variable,
    pub documents: Option<&'a str>,
}

fn release(suite: &conformance::Suite) -> String {
    let docs: HashMap<&str, &Value> = suite
        .documents
        .iter()
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    let mut seen: Vec<String> = vec![PolicyRun::MAIN.to_string()];
    let mut out = Vec::new();
    let mut at = 0;
    while at < seen.len() {
        let key = seen[at].clone();
        at += 1;
        let doc = match key.as_str() {
            PolicyRun::MAIN => &suite.content,
            other => match docs.get(other) {
                Some(d) => *d,
                None => continue,
            },
        };
        for import in doc
            .get("imports")
            .and_then(|i| i.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(name) = import.as_str() {
                if !seen.iter().any(|s| s == name) {
                    seen.push(name.to_string());
                }
            }
        }
        out.push(doc.clone());
    }
    Value::Array(out).to_string()
}

fn each(format: TraceFormat, sink: &mut dyn FnMut(Run)) {
    Spec::prepare();
    let traced = options(format);
    let (suites, _) = Spec::suites(Dialect::Graph);
    for suite in &suites {
        let Ok(run) = GraphRun::new(suite) else {
            continue;
        };
        let id = suite.id();
        for flag in [false, true] {
            let engine = engines(flag);
            for (case, c) in suite.cases.iter().enumerate() {
                let input = || c.input.depth_clone(64);
                let mut push = |variant: String, outcome: Value| {
                    sink(Run {
                        kind: "graph",
                        suite: &id,
                        case,
                        variant,
                        outcome,
                        request: &c.input,
                        documents: None,
                    })
                };
                push(
                    format!("{engine} decision"),
                    graph(&Guard::block(
                        run.uncompiled.evaluate_with_opts(input(), traced),
                    )),
                );
                push(
                    format!("{engine} precompiled"),
                    graph(&Guard::block(
                        run.compiled.evaluate_with_opts(input(), traced),
                    )),
                );
                push(
                    format!("{engine} engine"),
                    graph(&Guard::block(run.engine.evaluate_with_opts(
                        GraphRun::MAIN,
                        input(),
                        traced,
                    ))),
                );
                push(
                    format!("{engine} serialized"),
                    serial(Guard::block(
                        run.uncompiled.evaluate_serialized(input(), traced),
                    )),
                );
                push(
                    format!("{engine} engine serialized"),
                    serial(Guard::block(run.engine.evaluate_serialized(
                        GraphRun::MAIN,
                        input(),
                        traced,
                    ))),
                );
            }
        }
        engines(false);
        let inputs: Vec<_> = suite
            .cases
            .iter()
            .map(|c| c.input.depth_clone(64))
            .collect();
        let batch = Guard::block(run.uncompiled.evaluate_batch(&inputs, traced));
        let engine_batch = Guard::block(run.engine.evaluate_batch(GraphRun::MAIN, &inputs, traced));
        for (variant, results) in [("batch", batch), ("engine batch", engine_batch)] {
            for (case, result) in results.iter().enumerate() {
                sink(Run {
                    kind: "graph",
                    suite: &id,
                    case,
                    variant: variant.to_string(),
                    outcome: graph(result),
                    request: &suite.cases[case].input,
                    documents: None,
                });
            }
        }
    }
    let (suites, _) = Spec::suites(Dialect::Policy);
    for suite in &suites {
        let Ok(run) = PolicyRun::new(suite) else {
            continue;
        };
        let id = suite.id();
        let documents = release(suite);
        let request = |c: &conformance::Case| {
            let mut req = PolicyRun::request(c);
            req.trace = format;
            req
        };
        for flag in [false, true] {
            let engine = engines(flag);
            for (case, c) in suite.cases.iter().enumerate() {
                let mut push = |variant: String, outcome: Value| {
                    sink(Run {
                        kind: "policy",
                        suite: &id,
                        case,
                        variant,
                        outcome,
                        request: &c.input,
                        documents: Some(&documents),
                    })
                };
                push(
                    format!("{engine} workspace"),
                    policy(run.workspace.evaluate(&request(c))),
                );
                let mut enhanced = PolicyRun::request(c);
                if format == TraceFormat::V1 {
                    enhanced.trace = TraceFormat::V1;
                }
                push(
                    format!("{engine} enhanced"),
                    policy(run.workspace.enhance_trace(&enhanced)),
                );
                if c.goals.is_empty() {
                    push(
                        format!("{engine} engine"),
                        graph(&Guard::block(run.engine.evaluate_with_opts(
                            PolicyRun::MAIN,
                            c.input.depth_clone(64),
                            traced,
                        ))),
                    );
                    push(
                        format!("{engine} engine serialized"),
                        serial(Guard::block(run.engine.evaluate_serialized(
                            PolicyRun::MAIN,
                            c.input.depth_clone(64),
                            traced,
                        ))),
                    );
                }
            }
        }
        engines(false);
        let requests: Vec<_> = suite.cases.iter().map(request).collect();
        for (case, result) in run
            .workspace
            .evaluate_batch(&requests)
            .into_iter()
            .enumerate()
        {
            sink(Run {
                kind: "policy",
                suite: &id,
                case,
                variant: "workspace batch".into(),
                outcome: policy(result),
                request: &suite.cases[case].input,
                documents: Some(&documents),
            });
        }
        let plain: Vec<usize> = (0..suite.cases.len())
            .filter(|&i| suite.cases[i].goals.is_empty())
            .collect();
        let inputs: Vec<_> = plain
            .iter()
            .map(|&i| suite.cases[i].input.depth_clone(64))
            .collect();
        let batch = Guard::block(run.engine.evaluate_batch(PolicyRun::MAIN, &inputs, traced));
        for (at, result) in batch.iter().enumerate() {
            sink(Run {
                kind: "policy",
                suite: &id,
                case: plain[at],
                variant: "engine batch".into(),
                outcome: graph(result),
                request: &suite.cases[plain[at]].input,
                documents: Some(&documents),
            });
        }
    }
}

fn capture(path: &str) {
    type Group = ((String, String, usize), Vec<(String, Value, Vec<String>)>);
    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<(String, String, usize), usize> = HashMap::new();
    each(TraceFormat::Legacy, &mut |run| {
        let key = (run.kind.to_string(), run.suite.to_string(), run.case);
        let at = *index.entry(key.clone()).or_insert_with(|| {
            groups.push((key, Vec::new()));
            groups.len() - 1
        });
        let text = run.outcome.to_string();
        let outcomes = &mut groups[at].1;
        match outcomes.iter_mut().find(|(t, _, _)| *t == text) {
            Some((_, _, variants)) => variants.push(run.variant),
            None => outcomes.push((text, run.outcome, vec![run.variant])),
        }
    });
    let mut out = GzEncoder::new(std::fs::File::create(path).unwrap(), Compression::best());
    let mut written = 0usize;
    for ((kind, suite, case), outcomes) in &groups {
        for (_, outcome, variants) in outcomes {
            let line = json!({"kind": kind, "suite": suite, "case": case, "variants": variants, "outcome": outcome});
            writeln!(out, "{line}").unwrap();
            written += 1;
        }
    }
    out.finish().unwrap();
    eprintln!("{written} golden trace lines written to {path}");
}

fn parse(text: &str) -> Value {
    use serde::Deserialize;
    let mut de = serde_json::Deserializer::from_str(text);
    de.disable_recursion_limit();
    Value::deserialize(&mut de).unwrap()
}

type Key = (String, String, usize, String);

fn golden() -> HashMap<Key, Value> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/trace-base.jsonl.gz");
    let file = std::fs::File::open(path).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut flate2::read::GzDecoder::new(file), &mut text).unwrap();
    let mut out = HashMap::new();
    for line in text.lines() {
        let v = parse(line);
        let kind = v["kind"].as_str().unwrap().to_string();
        let suite = v["suite"].as_str().unwrap().to_string();
        let case = v["case"].as_u64().unwrap() as usize;
        for variant in v["variants"].as_array().unwrap() {
            out.insert(
                (
                    kind.clone(),
                    suite.clone(),
                    case,
                    variant.as_str().unwrap().to_string(),
                ),
                v["outcome"].clone(),
            );
        }
    }
    out
}

fn slot(outcome: &mut Value) -> Option<&mut Value> {
    let key = if outcome.get("ok").is_some() {
        "ok"
    } else {
        "err"
    };
    outcome
        .get_mut(key)?
        .get_mut("trace")
        .filter(|t| !t.is_null())
}

fn restored(run: &Run) -> Result<Value, String> {
    let mut outcome = run.outcome.clone();
    let Some(trace) = slot(&mut outcome) else {
        return Ok(outcome);
    };
    if trace.get("version") != Some(&json!(1)) {
        return Err(format!("not a version 1 document: {trace}"));
    }
    if trace.get("verbatim").is_some() {
        return Err("stored verbatim".to_string());
    }
    let back = zen_engine::trace::restore(zen_engine::trace::RestoreArgs {
        trace: zen_engine::trace::Input::Json(&trace.to_string()),
        request: zen_engine::trace::Input::Variable(run.request),
        documents: run.documents.map(zen_engine::trace::Input::Json),
    })
    .map_err(|e| format!("restore failed: {e}"))?;
    *trace = serde_json::to_value(&back).map_err(|e| e.to_string())?;
    Ok(canonical(outcome))
}

fn writes_sorted(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| {
                    let x = writes_sorted(x);
                    let x = match (k.as_str(), x) {
                        ("writes", Value::Array(mut items)) => {
                            items.sort_by_key(|i| i.to_string());
                            Value::Array(items)
                        }
                        (_, x) => x,
                    };
                    (k.clone(), x)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(writes_sorted).collect()),
        other => other.clone(),
    }
}

fn unhidden<'g>(
    want: &Value,
    got: &Value,
    serialized: impl FnOnce() -> Option<&'g Value>,
) -> Value {
    let hidden = want.pointer("/err/trace").is_some_and(Value::is_null)
        && got.pointer("/err/trace").is_some_and(|t| !t.is_null());
    let shown = hidden
        .then(serialized)
        .flatten()
        .and_then(|s| s.pointer("/err/trace"))
        .filter(|t| !t.is_null());
    let mut want = want.clone();
    if let (Some(trace), Some(slot)) = (shown, want.pointer_mut("/err/trace")) {
        *slot = trace.clone();
    }
    want
}

fn agrees(want: &Value, got: &Value) -> bool {
    want == got || writes_sorted(want) == writes_sorted(got)
}

const BATCH_INTERMEDIATE: [&str; 6] = [
    "graph/56-output-assembly.json5#leaves: a null in a later leaf deletes an earlier leaf's key",
    "graph/56-output-assembly.json5#leaves: a null in an earlier leaf does not delete a later leaf's key",
    "graph/56-output-assembly.json5#leaves: deep objects from three leaves merge key by key",
    "graph/57-table-output-shapes.json5#shapes: an object cell copying an input object, then adding a sibling column into it",
    "graph/70-generated-a.json5#ga011: 7 nodes, expr, expr, table*, table, switch, expr, table",
    "graph/71-generated-b.json5#gb069: 8 nodes, switch, switch*, switch, switch, expr, loop expr, expr, table",
];

fn without_trace(v: &Value) -> Value {
    let mut v = v.clone();
    for key in ["ok", "err"] {
        if let Some(Value::Object(o)) = v.get_mut(key) {
            o.remove("trace");
        }
    }
    v
}

fn dump_graph(
    out: &mut Option<(
        std::io::BufWriter<std::fs::File>,
        std::io::BufWriter<std::fs::File>,
    )>,
    id: &str,
    run: &Run,
) {
    let (Some((source, rust)), "graph") = (out.as_mut(), run.kind) else {
        return;
    };
    let mut outcome = run.outcome.clone();
    let Some(doc) = slot(&mut outcome) else {
        return;
    };
    let text = doc.to_string();
    let Ok(back) = zen_engine::trace::restore_json(zen_engine::trace::RestoreArgs {
        trace: zen_engine::trace::Input::Json(&text),
        request: zen_engine::trace::Input::Variable(run.request),
        documents: None,
    }) else {
        return;
    };
    let request = serde_json::to_value(run.request).unwrap();
    writeln!(
        source,
        "{}",
        json!({"id": id, "request": request, "trace": parse(&back)})
    )
    .unwrap();
    writeln!(rust, "{}", json!({"id": id, "v1": text})).unwrap();
}

fn compare(format: TraceFormat) {
    let golden = golden();
    let mut dump = std::env::var("TRACE_GRAPH_DUMP").ok().map(|dir| {
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str| {
            std::io::BufWriter::new(
                std::fs::File::create(std::path::Path::new(&dir).join(name)).unwrap(),
            )
        };
        (file("source.jsonl"), file("rust.jsonl"))
    });
    let mut failures: Vec<String> = Vec::new();
    let (mut semantics, mut intermediate) = (0usize, 0usize);
    let mut counts: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    each(format, &mut |run| {
        let label = format!("{} {} #{} {}", run.kind, run.suite, run.case, run.variant);
        if format == TraceFormat::V1 && !run.variant.starts_with("interpreted ") {
            dump_graph(&mut dump, &label, &run);
        }
        let tally = counts
            .entry(format!("{} {}", run.kind, run.variant))
            .or_default();
        tally.0 += 1;
        let key = |variant: &str| {
            (
                run.kind.to_string(),
                run.suite.to_string(),
                run.case,
                variant.to_string(),
            )
        };
        let Some(want) = golden.get(&key(&run.variant)) else {
            failures.push(format!("{label}: not in the golden set"));
            return;
        };
        let want = &unhidden(want, &run.outcome, || {
            let engine = run.variant.split(' ').next().unwrap_or("compiled");
            let engine = match engine {
                "interpreted" | "compiled" => engine,
                _ => "compiled",
            };
            golden.get(&key(&format!("{engine} serialized")))
        });
        let interpreted = run.variant.starts_with("interpreted ");
        let got = match (format, interpreted) {
            (TraceFormat::V1, true) => {
                let mut outcome = run.outcome.clone();
                if slot(&mut outcome).is_some_and(|t| t.get("version").is_some()) {
                    failures.push(format!(
                        "{label}: the interpreted engine wrote a v1 document"
                    ));
                    return;
                }
                outcome
            }
            (TraceFormat::V1, false) => match restored(&run) {
                Ok(v) => v,
                Err(e) => {
                    failures.push(format!("{label}: {e}"));
                    return;
                }
            },
            _ => run.outcome.clone(),
        };
        let batch = run.kind == "graph" && run.variant.ends_with("batch");
        if agrees(want, &got) {
            tally.1 += 1;
        } else if batch && !agrees(&without_trace(want), &without_trace(&got)) {
            semantics += 1;
        } else if batch && BATCH_INTERMEDIATE.contains(&run.suite) {
            intermediate += 1;
        } else {
            failures.push(format!("{label}: differs\n  want {want}\n  got  {got}"));
        }
    });
    eprintln!(
        "{format:?}: batch rows with the batch engine's own result {semantics}, with batch intermediate values {intermediate}"
    );
    for (k, (n, ok)) in &counts {
        eprintln!("{format:?} {k}: {ok}/{n}");
    }
    if let Ok(path) = std::env::var("TRACE_GOLDEN_FAILURES") {
        std::fs::write(path, failures.join("\n")).unwrap();
    }
    eprintln!("{format:?}: {} failures", failures.len());
    assert!(
        failures.is_empty(),
        "{}",
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn legacy_traces_equal_the_base_traces() {
    if cfg!(feature = "arbitrary_precision") {
        big_stack(|| compare(TraceFormat::Legacy));
    }
}

#[test]
fn v1_restores_to_the_base_traces() {
    if cfg!(feature = "arbitrary_precision") {
        big_stack(|| compare(TraceFormat::V1));
    }
}

#[test]
fn capture_base_traces() {
    if !cfg!(feature = "arbitrary_precision") {
        return;
    }
    let Ok(path) = std::env::var("TRACE_GOLDEN_WRITE") else {
        return;
    };
    big_stack(move || capture(&path));
}

fn gzip_len(text: &str) -> usize {
    let mut out = GzEncoder::new(Vec::new(), Compression::default());
    out.write_all(text.as_bytes()).unwrap();
    out.finish().unwrap().len()
}

fn pointers(v: &zen_engine::trace::testing::Val, at: &str, out: &mut Vec<String>) {
    use zen_engine::trace::testing::Val;
    if !at.is_empty() {
        out.push(at.to_string());
    }
    let mut push = |k: &str, x: &Val| {
        let seg = k.replace('~', "~0").replace('/', "~1");
        pointers(x, &format!("{at}/{seg}"), out);
    };
    match v {
        Val::Arr(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, x)| push(&i.to_string(), x)),
        Val::Obj(o) => o.iter().for_each(|(k, x)| push(k, x)),
        _ => {}
    }
}

fn redaction_failures(
    json: &str,
    input: &zen_engine::Variable,
    documents: Option<&str>,
    label: &str,
    failures: &mut Vec<String>,
) {
    let Some(plain) = zen_engine::trace::testing::unpacked(json) else {
        return;
    };
    let mut all = Vec::new();
    pointers(&plain, "", &mut all);
    let stride = match std::env::var("TRACE_REDACT_ALL") {
        Ok(_) => 1,
        Err(_) => (all.len() / 12).max(1),
    };
    for p in all.iter().step_by(stride) {
        let Ok(redacted) = zen_engine::trace::redact(json, &[p], None) else {
            continue;
        };
        if let Err(e) = zen_engine::trace::restore(zen_engine::trace::RestoreArgs {
            trace: zen_engine::trace::Input::Json(&redacted),
            request: zen_engine::trace::Input::Variable(input),
            documents: documents.map(zen_engine::trace::Input::Json),
        }) {
            failures.push(format!("{label}: redacting {p} does not restore: {e}"));
        }
    }
}

fn native_and_convert() {
    Spec::prepare();
    let misses = zen_engine::trace::testing::native_misses();
    let (suites, _) = Spec::suites(Dialect::Graph);
    let mut failures: Vec<String> = Vec::new();
    let (mut compared, mut native_raw, mut convert_raw, mut native_z, mut convert_z) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut worst: Vec<(f64, String)> = Vec::new();
    for suite in &suites {
        let Ok(run) = GraphRun::new(suite) else {
            continue;
        };
        for case in &suite.cases {
            let Some((native, convert)) = Guard::block(
                run.uncompiled
                    .evaluate_trace_pair(case.input.depth_clone(64)),
            ) else {
                continue;
            };
            compared += 1;
            let label = suite.location(case);
            for (name, doc) in [("native", &native), ("convert", &convert)] {
                if let Some(reason) = doc.verbatim_reason() {
                    failures.push(format!("{label}: {name} stored verbatim: {reason}"));
                }
            }
            let back = |doc: &zen_engine::trace::TraceDocument| {
                zen_engine::trace::restore(zen_engine::trace::RestoreArgs {
                    trace: zen_engine::trace::Input::Json(doc.json()),
                    request: zen_engine::trace::Input::Variable(&case.input),
                    documents: None,
                })
                .map(|v| serde_json::to_value(&v).unwrap())
                .map_err(|e| e.to_string())
            };
            redaction_failures(native.json(), &case.input, None, &label, &mut failures);
            match (back(&native), back(&convert)) {
                (Ok(a), Ok(b)) if a == b => {}
                (Ok(a), Ok(b)) => failures.push(format!(
                    "{label}: native and convert restore differently\n  native  {a}\n  convert {b}"
                )),
                (a, b) => failures.push(format!(
                    "{label}: restore failed: native {:?} convert {:?}",
                    a.err(),
                    b.err()
                )),
            }
            native_raw += native.json().len();
            convert_raw += convert.json().len();
            native_z += gzip_len(native.json());
            convert_z += gzip_len(convert.json());
            worst.push((
                native.json().len() as f64 / convert.json().len().max(1) as f64,
                label,
            ));
        }
    }
    let misses = zen_engine::trace::testing::native_misses() - misses;
    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    eprintln!(
        "{compared} traces compared, {misses} native fallbacks, {} failures",
        failures.len()
    );
    eprintln!(
        "raw: native {native_raw} B, convert {convert_raw} B ({:+.2}%); gzip: native {native_z} B, convert {convert_z} B ({:+.2}%)",
        100.0 * (native_raw as f64 / convert_raw as f64 - 1.0),
        100.0 * (native_z as f64 / convert_z as f64 - 1.0)
    );
    for (ratio, label) in worst.iter().take(5) {
        eprintln!(
            "  largest native/convert: {:+.1}% {label}",
            100.0 * (ratio - 1.0)
        );
    }
    assert!(compared > 0);
    assert_eq!(misses, 0, "native writer fell back");
    assert!(
        failures.is_empty(),
        "{}",
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(native_raw as f64 <= convert_raw as f64 * 1.02);
    assert!(native_z as f64 <= convert_z as f64 * 1.02);
}

#[test]
fn native_and_convert_restore_identically() {
    big_stack(native_and_convert);
}

fn untimed(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "perf" | "timings" | "timingColumns" | "msSinceRun"
                    )
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, x)| (k, untimed(x))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(untimed).collect()),
        other => other,
    }
}

fn raw_document(
    result: &Result<zen_engine::DecisionGraphResponse, Box<EvaluationError>>,
) -> Option<Value> {
    match result {
        Ok(r) => serde_json::to_value(r.trace.as_ref()?).ok(),
        Err(e) => match e.as_ref() {
            EvaluationError::NodeError { trace, .. } => serde_json::to_value(trace.as_ref()?).ok(),
            _ => None,
        },
    }
}

fn document(
    result: &Result<zen_engine::DecisionGraphResponse, Box<EvaluationError>>,
) -> Option<Value> {
    raw_document(result).map(untimed)
}

fn restored_of(
    result: &Result<zen_engine::DecisionGraphResponse, Box<EvaluationError>>,
    input: &zen_engine::Variable,
) -> Option<Value> {
    let text = serde_json::to_string(&raw_document(result)?).ok()?;
    let back = zen_engine::trace::restore(zen_engine::trace::RestoreArgs {
        trace: zen_engine::trace::Input::Json(&text),
        request: zen_engine::trace::Input::Variable(input),
        documents: None,
    })
    .ok()?;
    Some(unperformed(untimed(serde_json::to_value(&back).ok()?)))
}

fn unperformed(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(k, x)| !(k == "performance" && x.is_string()))
                .map(|(k, x)| (k, unperformed(x)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(unperformed).collect()),
        other => other,
    }
}

fn outcome_of(result: &Result<zen_engine::DecisionGraphResponse, Box<EvaluationError>>) -> Value {
    match result {
        Ok(r) => canonical(serde_json::to_value(&r.result).unwrap()),
        Err(e) => json!({"error": addresses(&e.to_string())}),
    }
}

fn batch_rows() {
    Spec::prepare();
    let previous = ZEN_CONFIG.compiled.swap(true, Ordering::Relaxed);
    let (suites, _) = Spec::suites(Dialect::Graph);
    let options = options(TraceFormat::V1);
    let (mut rows, mut equal, mut deviating, mut intermediate) = (0usize, 0usize, 0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();
    for suite in &suites {
        let Ok(run) = GraphRun::new(suite) else {
            continue;
        };
        let inputs: Vec<_> = suite
            .cases
            .iter()
            .map(|c| c.input.depth_clone(64))
            .collect();
        let batch = Guard::block(run.uncompiled.evaluate_batch(&inputs, options));
        for (case, got) in suite.cases.iter().zip(&batch) {
            rows += 1;
            let single = Guard::block(
                run.uncompiled
                    .evaluate_with_opts(case.input.depth_clone(64), options),
            );
            if outcome_of(&single) != outcome_of(got) {
                deviating += 1;
                continue;
            }
            match (document(&single), document(got)) {
                (a, b) if a == b => equal += 1,
                (Some(_), Some(_))
                    if restored_of(&single, &case.input)
                        .is_some_and(|a| restored_of(got, &case.input).is_some_and(|b| a == b)) =>
                {
                    equal += 1
                }
                _ if BATCH_INTERMEDIATE.contains(&suite.id().as_str()) => intermediate += 1,
                (a, b) => failures.push(format!(
                    "{}: batch row differs from the single-row trace\n  single {}\n  batch  {}",
                    suite.location(case),
                    a.map(|v| v.to_string()).unwrap_or_default(),
                    b.map(|v| v.to_string()).unwrap_or_default()
                )),
            }
        }
    }
    ZEN_CONFIG.compiled.store(previous, Ordering::Relaxed);
    eprintln!(
        "batch rows re-evaluated for their facts: {:?}",
        zen_engine::trace::testing::reevaluations()
    );
    let (columnar, rowwise) = zen_engine::trace::testing::batch_writes();
    eprintln!(
        "{rows} batch rows: {equal} equal to the single-row trace, {deviating} with a different result, {intermediate} with batch intermediate values, {} failures; documents written columnar {columnar}, row by row {rowwise}",
        failures.len()
    );
    if let Ok(path) = std::env::var("TRACE_BATCH_FAILURES") {
        std::fs::write(path, failures.join("\n")).unwrap();
    }
    assert!(
        failures.is_empty(),
        "{}",
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn batch_rows_equal_single_row_traces() {
    big_stack(batch_rows);
}

enum Owned {
    Dec(Vec<rust_decimal::Decimal>),
    Strs(Vec<&'static str>),
    Any(Vec<zen_engine::Variable>),
}

fn flat(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(map) if !map.is_empty() => {
            for (k, x) in map {
                if k.is_empty() || k.contains('.') {
                    out.push((String::new(), Value::Null));
                    continue;
                }
                let path = match prefix.is_empty() {
                    true => k.clone(),
                    false => format!("{prefix}.{k}"),
                };
                flat(&path, x, out);
            }
        }
        other => out.push((prefix.to_string(), other.clone())),
    }
}

type Built = (Vec<(&'static str, Owned, Vec<u64>)>, usize);

fn built(inputs: &[Value]) -> Option<Built> {
    let rows = inputs.len();
    let mut paths: Vec<String> = Vec::new();
    let mut cells: Vec<Vec<(String, Value)>> = Vec::with_capacity(rows);
    for input in inputs {
        if !matches!(input, Value::Object(map) if !map.is_empty()) {
            return None;
        }
        let mut leaves = Vec::new();
        flat("", input, &mut leaves);
        if leaves.iter().any(|(p, _)| p.is_empty()) {
            return None;
        }
        for (path, _) in &leaves {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        cells.push(leaves);
    }
    let overlapping = paths.iter().any(|a| {
        paths.iter().any(|b| {
            b.len() > a.len() && b.starts_with(a.as_str()) && b.as_bytes()[a.len()] == b'.'
        })
    });
    if overlapping {
        return None;
    }
    let mut columns = Vec::with_capacity(paths.len());
    for path in paths {
        let values: Vec<Option<&Value>> = cells
            .iter()
            .map(|leaves| leaves.iter().find(|(p, _)| *p == path).map(|(_, v)| v))
            .collect();
        let mut bits = vec![0u64; rows.div_ceil(64)];
        for (row, value) in values.iter().enumerate() {
            if value.is_some() {
                bits[row / 64] |= 1 << (row % 64);
            }
        }
        let present = values.iter().flatten();
        let owned = if present.clone().all(|v| v.is_number()) {
            let parsed: Option<Vec<rust_decimal::Decimal>> = values
                .iter()
                .map(|v| match v {
                    Some(v) => rust_decimal::Decimal::from_str_exact(&v.to_string()).ok(),
                    None => Some(rust_decimal::Decimal::ZERO),
                })
                .collect();
            match parsed {
                Some(parsed) => Owned::Dec(parsed),
                None => Owned::Any(
                    values
                        .iter()
                        .map(|v| v.map_or(zen_engine::Variable::Null, |v| v.clone().into()))
                        .collect(),
                ),
            }
        } else if present.clone().all(|v| v.is_string()) {
            Owned::Strs(
                values
                    .iter()
                    .map(|v| {
                        let text = v.and_then(|v| v.as_str()).unwrap_or_default();
                        &*Box::leak(text.to_string().into_boxed_str())
                    })
                    .collect(),
            )
        } else {
            Owned::Any(
                values
                    .iter()
                    .map(|v| v.map_or(zen_engine::Variable::Null, |v| v.clone().into()))
                    .collect(),
            )
        };
        let path: &'static str = Box::leak(path.into_boxed_str());
        columns.push((path, owned, bits));
    }
    Some((columns, rows))
}

fn columns_of(built: &Built) -> zen_engine::columns::Columns<'_> {
    use zen_engine::columns::{Column, Columns, Values};
    let (owned, rows) = built;
    let mut columns = Columns::new(*rows);
    for (path, values, bits) in owned {
        let values = match values {
            Owned::Dec(d) => Values::Dec(d),
            Owned::Strs(s) => Values::Strs(s),
            Owned::Any(a) => Values::Any(a),
        };
        columns = columns.column(path, Column::with_validity(values, bits, 0));
    }
    columns
}

fn columnar_document(output: &zen_engine::ColumnarOutput, row: usize) -> Option<Value> {
    use serde::Deserialize;
    let text = output.trace_json(row)?;
    let mut de = serde_json::Deserializer::from_str(&text);
    de.disable_recursion_limit();
    Value::deserialize(&mut de).ok()
}

fn restored_document(doc: &Value, input: &zen_engine::Variable) -> Option<Value> {
    let text = serde_json::to_string(doc).ok()?;
    let back = zen_engine::trace::restore(zen_engine::trace::RestoreArgs {
        trace: zen_engine::trace::Input::Json(&text),
        request: zen_engine::trace::Input::Variable(input),
        documents: None,
    })
    .ok()?;
    Some(unperformed(untimed(serde_json::to_value(&back).ok()?)))
}

const COLUMNAR_INTERMEDIATE: [&str; 3] = [
    "graph/71-generated-b.json5#gb018: 6 nodes, loop table, loop expr, loop expr, expr, loop expr, expr",
    "graph/71-generated-b.json5#gb008: 7 nodes, table*, switch*, switch, switch*, expr, expr, table",
    "graph/64-expression-optimizations.json5#alias: a copy taken before the original field is rewritten keeps the old value",
];

fn columnar_rows() {
    Spec::prepare();
    let previous = ZEN_CONFIG.compiled.swap(true, Ordering::Relaxed);
    let (suites, _) = Spec::suites(Dialect::Graph);
    let options = options(TraceFormat::V1);
    let (mut rows, mut equal, mut deviating, mut intermediate, mut skipped) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let (planned, batched) = zen_engine::trace::testing::columnar_traced();
    let mut failures: Vec<String> = Vec::new();
    for suite in &suites {
        let Ok(run) = GraphRun::new(suite) else {
            continue;
        };
        let inputs: Vec<Value> = suite.cases.iter().map(|c| c.input.to_value()).collect();
        let groups: Vec<(Built, Vec<usize>)> = match built(&inputs) {
            Some(all) => vec![(all, (0..inputs.len()).collect())],
            None => inputs
                .iter()
                .enumerate()
                .filter_map(|(at, input)| Some((built(std::slice::from_ref(input))?, vec![at])))
                .collect(),
        };
        skipped += inputs.len() - groups.iter().map(|(_, at)| at.len()).sum::<usize>();
        for (built, cases) in &groups {
            let columns = columns_of(built);
            let output = Guard::block(run.uncompiled.evaluate_columns(&columns, options));
            for (row, &at) in cases.iter().enumerate() {
                let case = &suite.cases[at];
                rows += 1;
                let request = columns.row(row);
                let single = Guard::block(
                    run.uncompiled
                        .evaluate_with_opts(request.depth_clone(64), options),
                );
                let got = match output.errors.get(row).and_then(Option::as_ref) {
                    Some(e) => json!({"error": addresses(&e.to_string())}),
                    None => canonical(output.row(row).to_value()),
                };
                if outcome_of(&single) != got {
                    deviating += 1;
                    continue;
                }
                let want = raw_document(&single);
                let have = columnar_document(&output, row);
                match (
                want.clone().map(untimed),
                have.clone().map(untimed),
            ) {
                (a, b) if a == b => equal += 1,
                (Some(_), Some(_))
                    if restored_document(want.as_ref().unwrap(), &request).is_some_and(|x| {
                        restored_document(have.as_ref().unwrap(), &request).is_some_and(|y| x == y)
                    }) =>
                {
                    equal += 1
                }
                _ if BATCH_INTERMEDIATE.contains(&suite.id().as_str())
                        || COLUMNAR_INTERMEDIATE.contains(&suite.id().as_str()) =>
                    {
                        intermediate += 1
                    }
                (a, b) => failures.push(format!(
                    "{}: columnar row differs from the single-row trace\n  single {}\n  columnar {}\n  restored single {}\n  restored columnar {}",
                    suite.location(case),
                    a.as_ref().map(|v| v.to_string()).unwrap_or_default(),
                    b.as_ref().map(|v| v.to_string()).unwrap_or_default(),
                    want.as_ref().and_then(|a| restored_document(a, &request)).map(|v| v.to_string()).unwrap_or_default(),
                    have.as_ref().and_then(|b| restored_document(b, &request)).map(|v| v.to_string()).unwrap_or_default(),
                )),
            }
            }
        }
    }
    ZEN_CONFIG.compiled.store(previous, Ordering::Relaxed);
    eprintln!(
        "columnar rows re-evaluated for their facts: {:?}",
        zen_engine::trace::testing::reevaluations()
    );
    let (now_planned, now_batched) = zen_engine::trace::testing::columnar_traced();
    eprintln!(
        "{rows} columnar rows ({skipped} not columnar): {equal} equal to the single-row trace, {deviating} with a different result, {intermediate} with batch intermediate values, {} failures; traced through the plan {}, through the batch engine {}",
        failures.len(),
        now_planned - planned,
        now_batched - batched
    );
    if let Ok(path) = std::env::var("TRACE_COLUMNAR_FAILURES") {
        std::fs::write(path, failures.join("\n")).unwrap();
    }
    assert!(
        failures.is_empty(),
        "{}",
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn columnar_rows_equal_single_row_traces() {
    big_stack(columnar_rows);
}

fn policy_native_and_convert() {
    Spec::prepare();
    let (suites, _) = Spec::suites(Dialect::Policy);
    let mut failures: Vec<String> = Vec::new();
    let (mut traces, mut native, mut native_raw, mut convert_raw, mut native_z, mut convert_z) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut dump = std::env::var("TRACE_POLICY_DUMP").ok().map(|dir| {
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str| {
            std::io::BufWriter::new(
                std::fs::File::create(std::path::Path::new(&dir).join(name)).unwrap(),
            )
        };
        (
            file("source.jsonl"),
            file("native.jsonl"),
            file("convert.jsonl"),
        )
    });
    for suite in &suites {
        let Ok(run) = PolicyRun::new(suite) else {
            continue;
        };
        let documents = release(suite);
        for case in &suite.cases {
            let mut req = PolicyRun::request(case);
            req.trace = TraceFormat::Legacy;
            let trace = match run.workspace.evaluate(&req) {
                Ok(r) => r.trace.and_then(|t| t.into_legacy()),
                Err(zen_engine::policy::EvaluationError::ExpressionFailed {
                    partial_trace,
                    ..
                }) => partial_trace.and_then(|t| t.into_legacy()),
                Err(_) => None,
            };
            let Some(trace) = trace else {
                continue;
            };
            traces += 1;
            let (fast, slow) =
                zen_engine::trace::testing::policy_pair(&trace, &case.input, &documents);
            let Some(fast) = fast else {
                continue;
            };
            native += 1;
            let back = |doc: &zen_engine::trace::TraceDocument| {
                zen_engine::trace::restore(zen_engine::trace::RestoreArgs {
                    trace: zen_engine::trace::Input::Json(doc.json()),
                    request: zen_engine::trace::Input::Variable(&case.input),
                    documents: Some(zen_engine::trace::Input::Json(&documents)),
                })
                .map(|v| canonical(serde_json::to_value(&v).unwrap()))
                .map_err(|e| e.to_string())
            };
            redaction_failures(
                fast.json(),
                &case.input,
                Some(&documents),
                &suite.location(case),
                &mut failures,
            );
            match (back(&fast), back(&slow)) {
                (Ok(a), Ok(b)) if agrees(&a, &b) => {}
                (a, b) => failures.push(format!(
                    "{}: native and convert restore differently\n  native  {:?}\n  convert {:?}",
                    suite.location(case),
                    a,
                    b
                )),
            }
            if let Some((source, natives, converts)) = dump.as_mut() {
                let id = format!("{}#{traces}", suite.location(case));
                let line = json!({
                    "id": id,
                    "request": serde_json::to_value(&case.input).unwrap(),
                    "documents": parse(&documents),
                    "trace": serde_json::to_value(&trace).unwrap(),
                });
                writeln!(source, "{line}").unwrap();
                writeln!(natives, "{}", json!({"id": id, "v1": fast.json()})).unwrap();
                writeln!(converts, "{}", json!({"id": id, "v1": slow.json()})).unwrap();
            }
            native_raw += fast.json().len();
            convert_raw += slow.json().len();
            native_z += gzip_len(fast.json());
            convert_z += gzip_len(slow.json());
        }
    }
    eprintln!(
        "{traces} policy traces, {native} written natively, {} failures",
        failures.len()
    );
    eprintln!(
        "raw: native {native_raw} B, convert {convert_raw} B ({:+.2}%); gzip: native {native_z} B, convert {convert_z} B ({:+.2}%)",
        100.0 * (native_raw as f64 / convert_raw.max(1) as f64 - 1.0),
        100.0 * (native_z as f64 / convert_z.max(1) as f64 - 1.0)
    );
    assert!(native > 0);
    assert!(
        failures.is_empty(),
        "{}",
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn policy_native_and_convert_restore_identically() {
    if cfg!(feature = "arbitrary_precision") {
        big_stack(policy_native_and_convert);
    }
}

struct Mix(u64);

impl Mix {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Timings {
    style: u64,
    at: u64,
    base: u64,
    step: u64,
    mix: Mix,
}

impl Timings {
    fn new(seed: u64) -> Self {
        let mut mix = Mix(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let style = mix.below(6);
        let base = [1, 400, 1_000, 25_000, 1_500_000, 2_000_000_000][mix.below(6) as usize];
        let step = [0, 1, 100, 1_000, 100_000][mix.below(5) as usize];
        Self {
            style,
            at: 0,
            base,
            step,
            mix,
        }
    }

    fn ns(&mut self) -> u64 {
        self.at += 1;
        match self.style {
            0 => self.base,
            1 => self.base + self.at * self.step,
            2 => self.mix.below(1_000),
            3 => {
                let digits = 1 + self.mix.below(10) as u32;
                self.mix.below(10u64.pow(digits))
            }
            4 => [self.base, self.base * 2][self.mix.below(2) as usize],
            _ => match self.mix.below(4) {
                0 => self.base,
                _ => self.mix.below(5_000_000),
            },
        }
    }

    fn text(&mut self) -> String {
        format!("{:.1?}", std::time::Duration::from_nanos(self.ns()))
    }
}

fn retimed(v: &Value, t: &mut Timings) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| match k.as_str() {
                    "performance" if x.is_null() => (k.clone(), Value::String(t.text())),
                    _ => (k.clone(), retimed(x, t)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|x| retimed(x, t)).collect()),
        other => other.clone(),
    }
}

fn widened(v: &Value, to: usize) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| match (k.as_str(), x) {
                    ("traceData", Value::Array(items))
                        if !items.is_empty() && items.iter().all(timed_nodes) =>
                    {
                        let items: Vec<Value> = (0..to.max(items.len()))
                            .map(|i| widened(&items[i % items.len()], to))
                            .collect();
                        (k.clone(), Value::Array(items))
                    }
                    _ => (k.clone(), widened(x, to)),
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|x| widened(x, to)).collect()),
        other => other.clone(),
    }
}

fn shuffled(v: &Value, mix: &mut Mix, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            for i in (1..entries.len()).rev() {
                entries.swap(i, mix.below(i as u64 + 1) as usize);
            }
            out.push('{');
            for (i, (k, x)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push(':');
                shuffled(x, mix, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                shuffled(x, mix, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn timed_nodes(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| o.values().any(|x| x.get("performance").is_some()))
}

fn nested(trace: &Value) -> bool {
    trace.as_object().is_some_and(|nodes| {
        nodes.values().any(|n| match n.get("traceData") {
            Some(Value::Array(items)) => items.iter().any(timed_nodes),
            Some(other) => timed_nodes(other),
            _ => false,
        })
    })
}

fn timing_stress(sets: u64) {
    let (suites, _) = Spec::suites(Dialect::Graph);
    let inputs: HashMap<String, &conformance::Suite> = suites.iter().map(|s| (s.id(), s)).collect();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/trace-base.jsonl.gz");
    let mut text = String::new();
    std::io::Read::read_to_string(
        &mut flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap()),
        &mut text,
    )
    .unwrap();
    let mut failures = Vec::new();
    let (mut cases, mut runs, mut columns) = (0usize, 0usize, 0usize);
    let mut seen = std::collections::HashSet::new();
    for line in text.lines() {
        let v = parse(line);
        if v["kind"] != "graph" {
            continue;
        }
        let Some(suite) = inputs.get(v["suite"].as_str().unwrap()) else {
            continue;
        };
        let case = v["case"].as_u64().unwrap() as usize;
        let (trace, error) = match (&v["outcome"]["ok"], &v["outcome"]["err"]) {
            (Value::Object(ok), _) => (ok.get("trace").cloned(), None),
            (_, Value::Object(err)) => (
                err.get("trace").cloned(),
                err.get("nodeId")
                    .and_then(|n| n.as_str())
                    .map(|n| (n.to_string(), err["source"].as_str().map(str::to_string))),
            ),
            _ => continue,
        };
        let Some(trace) = trace.filter(nested) else {
            continue;
        };
        if !seen.insert(trace.to_string()) {
            continue;
        }
        cases += 1;
        let request = &suite.cases[case].input;
        for seed in 0..sets {
            runs += 1;
            let source = match seed % 2 {
                0 => trace.clone(),
                _ => widened(&trace, 20),
            };
            let timed = retimed(&source, &mut Timings::new(seed + 1));
            let mut json = String::new();
            shuffled(&timed, &mut Mix(seed * 2 + 1), &mut json);
            let mut args = zen_engine::trace::ConvertArgs::new(
                zen_engine::trace::LegacyTrace::Json(&json),
                zen_engine::trace::Input::Variable(request),
            );
            args.verify = false;
            args.error = error
                .as_ref()
                .map(|(node, message)| zen_engine::trace::NodeError {
                    node,
                    message: message.as_deref(),
                });
            let label = format!("{} #{case} seed {seed}", v["suite"]);
            let doc = match zen_engine::trace::convert(args) {
                Ok(d) => d,
                Err(e) => {
                    failures.push(format!("{label}: convert failed: {e}"));
                    continue;
                }
            };
            columns += doc.json.contains("timingColumns") as usize;
            let back = zen_engine::trace::restore_json(zen_engine::trace::RestoreArgs {
                trace: zen_engine::trace::Input::Json(&doc.json),
                request: zen_engine::trace::Input::Variable(request),
                documents: None,
            });
            let want = zen_engine::trace::testing::parse(&json).unwrap();
            match back.map(|b| zen_engine::trace::testing::parse(&b).unwrap()) {
                Ok(b) => {
                    if let Some(at) = zen_engine::trace::testing::first_diff(&want, &b) {
                        failures.push(format!("{label}: differs at {at}\n  {json}"));
                    }
                }
                Err(e) => failures.push(format!("{label}: restore failed: {e}\n  {}", doc.json)),
            }
        }
    }
    eprintln!(
        "timing stress: {cases} loop-bearing traces, {runs} runs ({columns} with timingColumns), {} failures",
        failures.len()
    );
    assert!(cases > 0 && columns > 0);
    assert!(
        failures.is_empty(),
        "{}",
        failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn v1_restores_loop_traces_with_any_timings() {
    big_stack(|| timing_stress(64));
}

#[test]
fn trace_documents_move_across_threads() {
    fn sendable<T: Send + Sync + 'static>() {}
    sendable::<zen_engine::trace::TraceDocument>();
    Spec::prepare();
    engines(true);
    let (suites, _) = Spec::suites(Dialect::Policy);
    let mut moved = 0usize;
    for suite in suites.iter().take(40) {
        let Ok(run) = PolicyRun::new(suite) else {
            continue;
        };
        for case in &suite.cases {
            let mut request = PolicyRun::request(case);
            request.trace = TraceFormat::V1;
            let Ok(result) = run.workspace.evaluate(&request) else {
                continue;
            };
            let Some(doc) = result.trace.as_ref().and_then(|t| t.as_v1()).cloned() else {
                continue;
            };
            let copy = doc.clone();
            let there = std::thread::spawn(move || serde_json::to_string(&copy).unwrap())
                .join()
                .unwrap();
            let here = serde_json::to_string(&doc).unwrap();
            assert_eq!(here, there);
            moved += 1;
        }
    }
    engines(false);
    assert!(moved > 0);
}

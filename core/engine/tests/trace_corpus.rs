use rayon::prelude::*;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use zen_engine::trace::testing::{convert_val, first_diff, parse, restore_val, Val};
use zen_engine::trace::Status;

fn check_line(dir: &Path, line: &str) -> Result<(), String> {
    let rec = parse(line).map_err(|e| format!("record: {e}"))?;
    let id = rec
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_string();
    let request = rec.get("request").cloned().unwrap_or(Val::Null);
    let trace = rec.get("trace").cloned().unwrap_or(Val::Null);
    let error = rec.get("error").filter(|e| e.is_obj()).map(|e| {
        (
            e.get("node")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            e.get("message")
                .and_then(|x| x.as_str())
                .map(|x| x.to_string()),
        )
    });
    let docs = match (
        trace.get("executions"),
        rec.get("release").and_then(|r| r.as_str()),
    ) {
        (Some(Val::Arr(_)), Some(r)) => {
            let text = std::fs::read_to_string(dir.join("releases").join(format!("{r}.json")))
                .map_err(|e| format!("{id}: release {e}"))?;
            Some(parse(&text).map_err(|e| format!("{id}: release {e}"))?)
        }
        _ => None,
    };
    let err = error.as_ref().map(|(n, m)| (n.as_str(), m.as_deref()));
    let (text, status) = convert_val(&trace, &request, err, None, docs.as_ref());
    if let Status::Verbatim { reason } = status {
        return Err(format!("{id}: verbatim: {reason}"));
    }
    let back =
        restore_val(&text, &request, docs.as_ref()).map_err(|e| format!("{id}: restore: {e}"))?;
    if let Some(d) = first_diff(&trace, &back) {
        return Err(format!("{id}: differs at {d}"));
    }
    Ok(())
}

#[test]
#[ignore = "reads the trace-poc corpus; set ZEN_TRACE_CORPUS and run with --run-ignored"]
fn corpus_converts_and_restores_exactly() {
    let Ok(dir) = std::env::var("ZEN_TRACE_CORPUS") else {
        eprintln!("ZEN_TRACE_CORPUS is not set, skipping");
        return;
    };
    let dir = PathBuf::from(dir);
    let segments = std::env::var("ZEN_TRACE_SEGMENTS")
        .unwrap_or_else(|_| "spec,fraud,payload4k,x100,policy,policy-x100".to_string());
    let mut failures = Vec::new();
    for seg in segments.split(',').filter(|s| !s.is_empty()) {
        let file = std::fs::File::open(dir.join(format!("{seg}.jsonl"))).expect("segment file");
        let mut lines = BufReader::with_capacity(1 << 24, file).lines();
        let mut traces = 0usize;
        loop {
            let batch: Vec<String> = lines
                .by_ref()
                .take(512)
                .map(|l| l.expect("line"))
                .filter(|l| !l.is_empty())
                .collect();
            if batch.is_empty() {
                break;
            }
            traces += batch.len();
            let errs: Vec<String> = batch
                .par_iter()
                .filter_map(|l| check_line(&dir, l).err())
                .collect();
            failures.extend(errs);
        }
        eprintln!("{seg}: {traces} traces, {} failures so far", failures.len());
    }
    for f in failures.iter().take(20) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} failures", failures.len());
}

use mimalloc::MiMalloc;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;
use zen_engine::trace::testing::{convert_val_with, first_diff, parse, restore_val, Val};
use zen_engine::trace::Status;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

struct Args {
    corpus: PathBuf,
    segment: String,
    source: PathBuf,
    poc: Option<PathBuf>,
    poc_nodocs: Option<PathBuf>,
    out: Option<PathBuf>,
    threads: usize,
    limit: usize,
    no_docs: bool,
    zstd_today: bool,
    batch_mb: usize,
}

fn args() -> Args {
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    for a in std::env::args().skip(1) {
        let a = a.trim_start_matches("--").to_string();
        match a.split_once('=') {
            Some((k, v)) => m.insert(k.to_string(), v.to_string()),
            None => m.insert(a, "1".to_string()),
        };
    }
    let corpus = PathBuf::from(
        m.get("corpus")
            .cloned()
            .or_else(|| std::env::var("ZEN_TRACE_CORPUS").ok())
            .expect("--corpus or ZEN_TRACE_CORPUS"),
    );
    let segment = m.get("segment").cloned().expect("--segment");
    let source = m
        .get("legacy")
        .map(PathBuf::from)
        .unwrap_or_else(|| corpus.join(format!("{segment}.jsonl")));
    Args {
        corpus,
        segment,
        source,
        poc: m.get("poc").map(PathBuf::from),
        poc_nodocs: m.get("poc-nodocs").map(PathBuf::from),
        out: m.get("out").map(PathBuf::from),
        threads: m.get("threads").and_then(|t| t.parse().ok()).unwrap_or(0),
        limit: m
            .get("limit")
            .and_then(|t| t.parse().ok())
            .unwrap_or(usize::MAX),
        no_docs: m.contains_key("no-docs"),
        zstd_today: m.contains_key("zstd-today"),
        batch_mb: m
            .get("batch-mb")
            .and_then(|t| t.parse().ok())
            .unwrap_or(256),
    }
}

#[derive(Default, Clone)]
struct One {
    id: String,
    skipped: bool,
    poc_precision_only: bool,
    converted: bool,
    reason: String,
    round_trip: bool,
    poc_restore: Option<bool>,
    identical: Option<bool>,
    rust_raw: usize,
    rust_zstd: usize,
    poc_raw: usize,
    poc_zstd: usize,
    today_raw: usize,
    today_zstd: usize,
    encode_ns: u128,
    convert_ns: u128,
    restore_ns: u128,
    v1: String,
    nd: Option<NoDocs>,
    failure: Option<String>,
}

#[derive(Default, Clone)]
struct NoDocs {
    poc_precision_only: bool,
    converted: bool,
    round_trip: bool,
    poc_restore: Option<bool>,
    identical: Option<bool>,
    rust_raw: usize,
    rust_zstd: usize,
    poc_raw: usize,
    poc_zstd: usize,
    v1: String,
}

fn zsize(s: &str) -> usize {
    zstd::bulk::compress(s.as_bytes(), 3)
        .map(|b| b.len())
        .unwrap_or(0)
}

fn release_text(
    corpus: &std::path::Path,
    id: &str,
    cache: &Mutex<BTreeMap<String, std::sync::Arc<String>>>,
) -> std::sync::Arc<String> {
    if let Some(t) = cache.lock().unwrap().get(id) {
        return t.clone();
    }
    let t = std::sync::Arc::new(
        std::fs::read_to_string(corpus.join("releases").join(format!("{id}.json")))
            .expect("release"),
    );
    cache.lock().unwrap().insert(id.to_string(), t.clone());
    t
}

fn poc_text(line: &str) -> (String, String) {
    let v = parse(line).expect("poc line");
    (
        v.get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        v.get("v1")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    )
}

fn f64_of(d: &rust_decimal::Decimal) -> f64 {
    d.to_string().parse().unwrap_or(f64::NAN)
}

fn eq_f64(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => f64_of(x) == f64_of(y),
        (Val::Arr(x), Val::Arr(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| eq_f64(p, q))
        }
        (Val::Obj(x), Val::Obj(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| eq_f64(v, w)))
        }
        _ => zen_engine::trace::testing::eq(a, b),
    }
}

fn check(
    a: &Args,
    line: &str,
    poc: Option<&str>,
    poc_nd: Option<&str>,
    cache: &Mutex<BTreeMap<String, std::sync::Arc<String>>>,
) -> One {
    let rec = match parse(line) {
        Ok(r) => r,
        Err(e) => {
            let id = line
                .split("\"id\":\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .unwrap_or("?")
                .to_string();
            return One {
                id: id.clone(),
                skipped: true,
                failure: Some(format!("{id}: not readable as Zen numbers: {e}")),
                ..Default::default()
            };
        }
    };
    let id = rec
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
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
    let err = error.as_ref().map(|(n, m)| (n.as_str(), m.as_deref()));
    let is_policy = trace.get("executions").is_some_and(|e| e.is_arr());
    let docs = if is_policy {
        rec.get("release")
            .and_then(|r| r.as_str())
            .map(|r| parse(&release_text(&a.corpus, r, cache)).expect("release json"))
    } else {
        None
    };
    let mut one = One {
        id: id.clone(),
        ..Default::default()
    };
    let today = line.len();
    one.today_raw = today;
    if a.zstd_today {
        one.today_zstd = zsize(line);
    }
    let t0 = Instant::now();
    let _ = convert_val_with(&trace, &request, err, None, docs.as_ref(), false);
    one.encode_ns = t0.elapsed().as_nanos();
    let t0 = Instant::now();
    let (text, status) = convert_val_with(&trace, &request, err, None, docs.as_ref(), true);
    one.convert_ns = t0.elapsed().as_nanos();
    one.converted = status == Status::Converted;
    if let Status::Verbatim { reason } = &status {
        one.reason = reason.clone();
        one.failure = Some(format!("{id}: verbatim: {reason}"));
    }
    let t0 = Instant::now();
    let back = restore_val(&text, &request, docs.as_ref());
    one.restore_ns = t0.elapsed().as_nanos();
    match back {
        Ok(b) => match first_diff(&trace, &b) {
            None => one.round_trip = true,
            Some(d) => one.failure = Some(format!("{id}: rust restore differs at {d}")),
        },
        Err(e) => one.failure = Some(format!("{id}: rust restore failed: {e}")),
    }
    one.rust_raw = text.len();
    one.rust_zstd = zsize(&text);
    if let Some(p) = poc {
        let (pid, ptext) = poc_text(p);
        assert_eq!(pid, id, "poc file out of order");
        one.poc_raw = ptext.len();
        one.poc_zstd = zsize(&ptext);
        one.identical = Some(ptext == text);
        match restore_val(&ptext, &request, docs.as_ref()) {
            Ok(b) => match first_diff(&trace, &b) {
                None => one.poc_restore = Some(true),
                Some(d) => {
                    one.poc_restore = Some(false);
                    one.poc_precision_only = eq_f64(&trace, &b);
                    if !one.poc_precision_only {
                        one.failure
                            .get_or_insert(format!("{id}: restore of poc text differs at {d}"));
                    }
                }
            },
            Err(e) => {
                one.poc_restore = Some(false);
                one.failure
                    .get_or_insert(format!("{id}: restore of poc text failed: {e}"));
            }
        }
    }
    if a.no_docs && is_policy {
        let (t2, s2) = convert_val_with(&trace, &request, err, None, None, true);
        let mut nd = NoDocs {
            converted: s2 == Status::Converted,
            ..Default::default()
        };
        nd.round_trip =
            restore_val(&t2, &request, None).is_ok_and(|b| first_diff(&trace, &b).is_none());
        nd.rust_raw = t2.len();
        nd.rust_zstd = zsize(&t2);
        if let Some(p) = poc_nd {
            let (pid, ptext) = poc_text(p);
            assert_eq!(pid, id, "poc no-docs file out of order");
            nd.poc_raw = ptext.len();
            nd.poc_zstd = zsize(&ptext);
            nd.identical = Some(ptext == t2);
            let back = restore_val(&ptext, &request, None);
            nd.poc_restore = Some(back.as_ref().is_ok_and(|b| first_diff(&trace, b).is_none()));
            nd.poc_precision_only =
                nd.poc_restore == Some(false) && back.as_ref().is_ok_and(|b| eq_f64(&trace, b));
        }
        if !(nd.converted
            && nd.round_trip
            && (nd.poc_restore != Some(false) || nd.poc_precision_only))
        {
            one.failure.get_or_insert(format!(
                "{id}: without documents: converted {} round trip {} poc {:?}",
                nd.converted, nd.round_trip, nd.poc_restore
            ));
        }
        nd.v1 = t2;
        one.nd = Some(nd);
    }
    one.v1 = text;
    one
}

#[derive(Default)]
struct Sum {
    traces: usize,
    skipped: usize,
    poc_precision_only: usize,
    converted: usize,
    round_trip: usize,
    poc_checked: usize,
    poc_restore: usize,
    identical: usize,
    rust_raw: usize,
    rust_zstd: usize,
    poc_raw: usize,
    poc_zstd: usize,
    today_raw: usize,
    today_zstd: usize,
    encode_ns: u128,
    convert_ns: u128,
    restore_ns: u128,
    max_convert_ns: u128,
    max_convert_id: String,
    nd_traces: usize,
    nd_precision_only: usize,
    nd_ok: usize,
    nd_poc_restore: usize,
    nd_identical: usize,
    nd_rust_raw: usize,
    nd_rust_zstd: usize,
    nd_poc_raw: usize,
    nd_poc_zstd: usize,
    failures: Vec<String>,
    verbatim: BTreeMap<String, usize>,
}

fn main() {
    let a = args();
    if a.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(a.threads)
            .build_global()
            .unwrap();
    }
    let src = BufReader::with_capacity(1 << 24, std::fs::File::open(&a.source).expect("source"));
    let mut poc = a
        .poc
        .as_ref()
        .map(|p| BufReader::with_capacity(1 << 22, std::fs::File::open(p).expect("poc")).lines());
    let mut poc_nd = a.poc_nodocs.as_ref().map(|p| {
        BufReader::with_capacity(1 << 22, std::fs::File::open(p).expect("poc nodocs")).lines()
    });
    let mut out = a
        .out
        .as_ref()
        .map(|p| BufWriter::new(std::fs::File::create(p).expect("out")));
    let cache = Mutex::new(BTreeMap::new());
    let mut sum = Sum::default();
    let wall = Instant::now();
    let mut lines = src.lines();
    let mut n = 0usize;
    loop {
        let mut batch: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        let mut bytes = 0usize;
        while batch.len() < 256 && bytes < (a.batch_mb << 20) && n < a.limit {
            let Some(l) = lines.next() else { break };
            let l = l.expect("line");
            if l.is_empty() {
                continue;
            }
            n += 1;
            bytes += l.len();
            let p = poc
                .as_mut()
                .and_then(|p| p.next())
                .map(|x| x.expect("poc line"));
            let pn = poc_nd
                .as_mut()
                .and_then(|p| p.next())
                .map(|x| x.expect("poc line"));
            batch.push((l, p, pn));
        }
        if batch.is_empty() {
            break;
        }
        let results: Vec<One> = batch
            .par_iter()
            .map(|(l, p, pn)| check(&a, l, p.as_deref(), pn.as_deref(), &cache))
            .collect();
        for r in results {
            if r.skipped {
                sum.skipped += 1;
                if let Some(f) = &r.failure {
                    sum.failures.push(f.clone());
                }
                continue;
            }
            sum.traces += 1;
            sum.poc_precision_only += r.poc_precision_only as usize;
            sum.converted += r.converted as usize;
            sum.round_trip += r.round_trip as usize;
            if let Some(ok) = r.poc_restore {
                sum.poc_checked += 1;
                sum.poc_restore += ok as usize;
            }
            sum.identical += r.identical.unwrap_or(false) as usize;
            sum.rust_raw += r.rust_raw;
            sum.rust_zstd += r.rust_zstd;
            sum.poc_raw += r.poc_raw;
            sum.poc_zstd += r.poc_zstd;
            sum.today_raw += r.today_raw;
            sum.today_zstd += r.today_zstd;
            sum.encode_ns += r.encode_ns;
            sum.convert_ns += r.convert_ns;
            sum.restore_ns += r.restore_ns;
            if r.convert_ns > sum.max_convert_ns {
                sum.max_convert_ns = r.convert_ns;
                sum.max_convert_id = r.id.clone();
            }
            if !r.converted {
                *sum.verbatim
                    .entry(r.reason.chars().take(80).collect())
                    .or_default() += 1;
            }
            if let Some(f) = &r.failure {
                if sum.failures.len() < 20 {
                    sum.failures.push(f.chars().take(400).collect());
                }
            }
            if let Some(nd) = &r.nd {
                sum.nd_traces += 1;
                sum.nd_precision_only += nd.poc_precision_only as usize;
                sum.nd_ok += (nd.converted && nd.round_trip) as usize;
                sum.nd_poc_restore += nd.poc_restore.unwrap_or(false) as usize;
                sum.nd_identical += nd.identical.unwrap_or(false) as usize;
                sum.nd_rust_raw += nd.rust_raw;
                sum.nd_rust_zstd += nd.rust_zstd;
                sum.nd_poc_raw += nd.poc_raw;
                sum.nd_poc_zstd += nd.poc_zstd;
            }
            if let Some(o) = out.as_mut() {
                let mut line = format!(
                    "{{\"id\":{},\"v1\":{}",
                    Val::str(&r.id).to_json(),
                    Val::str(&r.v1).to_json()
                );
                if let Some(nd) = &r.nd {
                    line.push_str(&format!(",\"v1NoDocs\":{}", Val::str(&nd.v1).to_json()));
                }
                line.push('}');
                writeln!(o, "{line}").unwrap();
            }
        }
    }
    let t = sum.traces.max(1) as f64;
    let ms = |ns: u128| ns as f64 / 1e6 / t;
    let pct = |a: usize, b: usize| {
        if b == 0 {
            0.0
        } else {
            (a as f64 / b as f64 - 1.0) * 100.0
        }
    };
    let mut report = format!(
        "{{\"segment\":\"{}\",\"skipped\":{},\"pocDiffersOnlyBeyondDoublePrecision\":{},\"traces\":{},\"converted\":{},\"verbatim\":{},\"roundTrip\":{},\"pocChecked\":{},\"restoreOfPocText\":{},\"byteIdentical\":{},\
\"bytesPerTrace\":{{\"rustRaw\":{:.1},\"rustZstd\":{:.1},\"pocRaw\":{:.1},\"pocZstd\":{:.1},\"todayRaw\":{:.1},\"todayZstd\":{:.1},\"rawVsPocPct\":{:.3},\"zstdVsPocPct\":{:.3}}},\
\"msPerTrace\":{{\"encode\":{:.4},\"convertWithVerify\":{:.4},\"restore\":{:.4},\"maxConvert\":{:.1},\"maxConvertId\":\"{}\"}},\"wallSeconds\":{:.1}",
        a.segment,
        sum.skipped,
        sum.poc_precision_only,
        sum.traces,
        sum.converted,
        sum.traces - sum.converted,
        sum.round_trip,
        sum.poc_checked,
        sum.poc_restore,
        sum.identical,
        sum.rust_raw as f64 / t,
        sum.rust_zstd as f64 / t,
        sum.poc_raw as f64 / t,
        sum.poc_zstd as f64 / t,
        sum.today_raw as f64 / t,
        sum.today_zstd as f64 / t,
        pct(sum.rust_raw, sum.poc_raw),
        pct(sum.rust_zstd, sum.poc_zstd),
        ms(sum.encode_ns),
        ms(sum.convert_ns),
        ms(sum.restore_ns),
        sum.max_convert_ns as f64 / 1e6,
        sum.max_convert_id,
        wall.elapsed().as_secs_f64()
    );
    if sum.nd_traces > 0 {
        let tn = sum.nd_traces as f64;
        report.push_str(&format!(
            ",\"withoutDocuments\":{{\"traces\":{},\"ok\":{},\"pocDiffersOnlyBeyondDoublePrecision\":{},\"restoreOfPocText\":{},\"byteIdentical\":{},\"rustRaw\":{:.1},\"rustZstd\":{:.1},\"pocRaw\":{:.1},\"pocZstd\":{:.1}}}",
            sum.nd_traces,
            sum.nd_ok,
            sum.nd_precision_only,
            sum.nd_poc_restore,
            sum.nd_identical,
            sum.nd_rust_raw as f64 / tn,
            sum.nd_rust_zstd as f64 / tn,
            sum.nd_poc_raw as f64 / tn,
            sum.nd_poc_zstd as f64 / tn
        ));
    }
    let fails: Vec<String> = sum.failures.iter().map(|f| Val::str(f).to_json()).collect();
    let verb: Vec<String> = sum
        .verbatim
        .iter()
        .map(|(k, v)| format!("{}:{}", Val::str(k).to_json(), v))
        .collect();
    report.push_str(&format!(
        ",\"verbatimReasons\":{{{}}},\"failures\":[{}]}}",
        verb.join(","),
        fails.join(",")
    ));
    println!("{report}");
    if let Some(o) = out.as_mut() {
        o.flush().unwrap();
    }
}

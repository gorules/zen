use std::panic::{catch_unwind, AssertUnwindSafe};
use zen_engine::trace::testing::{parse, restore_val, Val};
use zen_engine::trace::{
    convert, inspect, parse_document, redact, restore, restore_json, ConvertArgs, Input,
    LegacyTrace, NodeError, RestoreArgs,
};

const ROUNDS: usize = 16;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn count(v: &Val) -> usize {
    1 + match v {
        Val::Arr(a) => a.iter().map(count).sum(),
        Val::Obj(o) => o.values().map(count).sum(),
        _ => 0,
    }
}

fn replace(v: &Val, target: &mut usize, with: &Val) -> Val {
    if *target == 0 {
        *target = usize::MAX;
        return with.clone();
    }
    if *target != usize::MAX {
        *target -= 1;
    }
    match v {
        Val::Arr(a) => Val::arr(a.iter().map(|x| replace(x, target, with)).collect()),
        Val::Obj(o) => {
            let mut out = zen_engine::trace::testing::Obj::new();
            for (k, x) in o.iter() {
                out.insert(k.clone(), replace(x, target, with));
            }
            Val::obj(out)
        }
        _ => v.clone(),
    }
}

#[test]
fn damaged_documents_are_refused_without_panicking() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/trace/poc.jsonl"
    );
    let text = std::fs::read_to_string(path).unwrap();
    let swaps: Vec<Val> = [
        "null",
        "0",
        "-1",
        "1.5",
        "\"x\"",
        "\"/a/0\"",
        "[]",
        "{}",
        "[0,1]",
        "{\"$ref\":\"self/nodes\"}",
        "{\"$runs\":[[1,5,1]]}",
        "100000000000",
    ]
    .iter()
    .map(|s| parse(s).unwrap())
    .collect();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut tried = 0;
    for line in text.lines().step_by(3) {
        let f = parse(line).unwrap();
        let request = f.get("request").cloned().unwrap_or(Val::Null);
        let docs = f.get("documents").cloned();
        let doc = parse(f.get("poc").unwrap().as_str().unwrap()).unwrap();
        let n = count(&doc);
        for _ in 0..ROUNDS {
            let mut at = (rng.next() as usize) % n;
            let with = &swaps[(rng.next() as usize) % swaps.len()];
            let damaged = replace(&doc, &mut at, with).to_json();
            let r = catch_unwind(AssertUnwindSafe(|| {
                restore_val(&damaged, &request, docs.as_ref()).map(|_| ())
            }));
            assert!(r.is_ok(), "panic on {}", &damaged[..damaged.len().min(400)]);
            tried += 1;
        }
    }
    assert!(tried > 1000);
}

struct Indexed {
    legacy: Vec<Val>,
    request: Vec<Val>,
    docs: Vec<Vec<Val>>,
    documents: Option<Vec<Val>>,
}

struct Case {
    legacy: Val,
    request: Val,
    documents: Option<Val>,
    docs: Vec<String>,
    error: Option<(String, Option<String>)>,
}

fn str_field(v: &Val, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(str::to_string)
}

fn cases() -> Vec<Case> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/trace");
    let mut out = Vec::new();
    for (file, legacy_key, doc_keys) in [
        ("poc.jsonl", "trace", &["poc", "pocNoDocs"][..]),
        ("additions.jsonl", "legacy", &["doc"][..]),
    ] {
        let text = std::fs::read_to_string(format!("{dir}/{file}")).unwrap();
        for line in text.lines().filter(|l| !l.is_empty()) {
            let f = parse(line).unwrap();
            let error = f.get("error").filter(|e| e.is_obj()).map(|e| {
                (
                    str_field(e, "node").unwrap_or_default(),
                    str_field(e, "message"),
                )
            });
            out.push(Case {
                legacy: f.get(legacy_key).cloned().unwrap_or(Val::Null),
                request: f.get("request").cloned().unwrap_or(Val::Null),
                documents: f.get("documents").cloned(),
                docs: doc_keys
                    .iter()
                    .filter_map(|k| match f.get(k) {
                        Some(Val::Str(s)) => Some(s.to_string()),
                        Some(v) if v.is_obj() => Some(v.to_json()),
                        _ => None,
                    })
                    .collect(),
                error,
            });
        }
    }
    out
}

fn nodes(v: &Val, out: &mut Vec<Val>) {
    out.push(v.clone());
    match v {
        Val::Arr(a) => a.iter().for_each(|x| nodes(x, out)),
        Val::Obj(o) => o.values().for_each(|x| nodes(x, out)),
        _ => {}
    }
}

enum Edit<'a> {
    Replace(&'a Val),
    Remove,
    Truncate(usize),
}

fn edit(v: &Val, target: &mut usize, how: &Edit) -> Option<Val> {
    if *target == 0 {
        *target = usize::MAX;
        return match how {
            Edit::Replace(with) => Some((*with).clone()),
            Edit::Remove => None,
            Edit::Truncate(keep) => Some(match v {
                Val::Arr(a) => Val::arr(a.iter().take(*keep).cloned().collect()),
                Val::Obj(o) => {
                    let mut out = zen_engine::trace::testing::Obj::new();
                    for (k, x) in o.iter().take(*keep) {
                        out.insert(k.clone(), x.clone());
                    }
                    Val::obj(out)
                }
                Val::Str(s) => Val::str(&s.chars().take(*keep).collect::<String>()),
                other => other.clone(),
            }),
        };
    }
    if *target != usize::MAX {
        *target -= 1;
    }
    Some(match v {
        Val::Arr(a) => Val::arr(a.iter().filter_map(|x| edit(x, target, how)).collect()),
        Val::Obj(o) => {
            let mut out = zen_engine::trace::testing::Obj::new();
            for (k, x) in o.iter() {
                if let Some(y) = edit(x, target, how) {
                    out.insert(k.clone(), y);
                }
            }
            Val::obj(out)
        }
        _ => v.clone(),
    })
}

fn retyped(v: &Val, pick: u64) -> Val {
    match (v, pick % 3) {
        (Val::Obj(o), 0) => Val::arr(o.values().cloned().collect()),
        (Val::Arr(a), 0) => {
            let mut out = zen_engine::trace::testing::Obj::new();
            for (i, x) in a.iter().enumerate() {
                out.insert(std::rc::Rc::from(i.to_string().as_str()), x.clone());
            }
            Val::obj(out)
        }
        (Val::Num(n), 0) => Val::str(&n.to_string()),
        (Val::Num(n), 1) => {
            parse(&format!("{}", n * rust_decimal::Decimal::from(1000))).unwrap_or(Val::Null)
        }
        (Val::Num(n), _) => {
            parse(&format!("{}", -*n - rust_decimal::Decimal::ONE)).unwrap_or(Val::Null)
        }
        (Val::Str(s), 0) => parse(s).unwrap_or_else(|_| Val::int(s.len() as i64)),
        (Val::Str(s), 1) => Val::str(&format!("/{s}/0")),
        (Val::Str(s), _) => Val::str(&format!("@{}", s.len())),
        (Val::Bool(b), _) => Val::str(&b.to_string()),
        (other, _) => other.clone(),
    }
}

const SWAPS: [&str; 26] = [
    "null",
    "true",
    "0",
    "-1",
    "1.5",
    "4294967296",
    "100000000000",
    "1e20",
    "\"\"",
    "\"x\"",
    "\"/\"",
    "\"/a/0\"",
    "\"/nodes/0\"",
    "\"@0\"",
    "\"@99999\"",
    "\"previous\"",
    "\"1.2ms\"",
    "[]",
    "{}",
    "[0,1]",
    "{\"$ref\":\"self/nodes\"}",
    "{\"$runs\":[[1,5,1]]}",
    "{\"$pick\":[1],\"at\":[3]}",
    "{\"path\":0,\"iterations\":3}",
    "{\"iterationPaths\":[0,7],\"timings\":[[1]]}",
    "{\"$each\":{\"/x\":{\"$set\":{\"/y\":[1]}}}}",
];

const TYPES: [&str; 6] = ["null", "true", "7", "\"x\"", "[]", "{}"];

fn mutate(v: &Val, all: &[Val], swaps: &[Val], rng: &mut Rng) -> Val {
    if all.is_empty() {
        return v.clone();
    }
    let n = all.len();
    let mut at = (rng.next() as usize) % n;
    let pick = rng.next();
    let how = match pick % 8 {
        0..=2 => Edit::Replace(&swaps[(rng.next() as usize) % swaps.len()]),
        3 => Edit::Remove,
        4 => Edit::Truncate((rng.next() % 4) as usize),
        5 => Edit::Replace(&all[(rng.next() as usize) % n]),
        _ => {
            let with = retyped(&all[at % n], rng.next());
            return edit(v, &mut at, &Edit::Replace(&with)).unwrap_or(Val::Null);
        }
    };
    edit(v, &mut at, &how).unwrap_or(Val::Null)
}

fn cut(text: &str, rng: &mut Rng) -> String {
    let mut end = (rng.next() as usize) % text.len().max(1);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[derive(Default)]
struct Tally {
    ok: usize,
    err: usize,
}

impl Tally {
    fn add<T, E>(&mut self, r: &Result<T, E>) {
        match r {
            Ok(_) => self.ok += 1,
            Err(_) => self.err += 1,
        }
    }
}

fn run_case(
    tally: &mut Tally,
    case: &Case,
    legacy: &str,
    request: &str,
    documents: Option<&str>,
    doc: Option<&str>,
    mode: u64,
) {
    let error = case.error.as_ref().map(|(node, message)| NodeError {
        node,
        message: message.as_deref(),
    });
    let legacy_var = parse_document(legacy).ok();
    let request_var = parse_document(request).ok();
    let reference = legacy_var
        .as_ref()
        .map(|v| serde_json::to_string(&v.serialize_ref()).unwrap_or_default());
    let trace = match (mode % 3, &legacy_var, &reference) {
        (1, Some(v), _) => LegacyTrace::Variable(v),
        (2, _, Some(r)) => LegacyTrace::ReferenceString(r),
        _ => LegacyTrace::Json(legacy),
    };
    let request_in = match (mode % 2, &request_var) {
        (1, Some(v)) => Input::Variable(v),
        _ => Input::Json(request),
    };
    let converted = convert(ConvertArgs {
        trace,
        request: request_in,
        error,
        engine_version: Some("fuzz"),
        documents: documents.map(Input::Json),
        verify: mode % 4 != 3,
        evaluation: None,
    });
    tally.add(&converted);
    let own = converted.ok().map(|o| o.json);
    for text in doc.into_iter().chain(own.as_deref()) {
        let doc_var = parse_document(text).ok();
        let trace = match (mode % 2, &doc_var) {
            (0, Some(v)) => Input::Variable(v),
            _ => Input::Json(text),
        };
        tally.add(&restore(RestoreArgs {
            trace,
            request: request_in,
            documents: documents.map(Input::Json),
        }));
        tally.add(&restore_json(RestoreArgs {
            trace: Input::Json(text),
            request: Input::Json(request),
            documents: None,
        }));
        let _ = inspect(text);
        let _ = redact(
            text,
            &["/nodes/0/output", "/executions/0/writes"],
            Some("fuzz"),
        );
    }
}

fn fuzz_cases() -> usize {
    std::env::var("ZEN_TRACE_FUZZ_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(6_000)
}

#[test]
fn convert_and_restore_never_panic_on_mutated_inputs() {
    let cases = cases();
    let swaps: Vec<Val> = SWAPS.iter().map(|s| parse(s).unwrap()).collect();
    let types: Vec<Val> = TYPES.iter().map(|s| parse(s).unwrap()).collect();
    let seed = std::env::var("ZEN_TRACE_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x2545_f491_4f6c_dd1d_u64);
    let mut rng = Rng(seed);
    let total = fuzz_cases();
    let mut failures: Vec<String> = Vec::new();
    let mut ran = 0usize;
    let mut tally = Tally::default();
    let mut check = |case: &Case,
                     legacy: &Val,
                     request: &Val,
                     documents: Option<&Val>,
                     doc: Option<String>,
                     mode: u64,
                     failures: &mut Vec<String>| {
        let (legacy, request) = (legacy.to_json(), request.to_json());
        let documents = documents.map(|d| d.to_json());
        let r = catch_unwind(AssertUnwindSafe(|| {
            run_case(
                &mut tally,
                case,
                &legacy,
                &request,
                documents.as_deref(),
                doc.as_deref(),
                mode,
            )
        }));
        if r.is_err() && failures.len() < 10 {
            failures.push(format!(
                "mode {mode}\nlegacy {}\nrequest {}\ndoc {}",
                &legacy[..legacy.len().min(600)],
                &request[..request.len().min(300)],
                doc.as_deref().map_or("", |d| &d[..d.len().min(600)])
            ));
        }
    };
    let systematic = cases
        .iter()
        .filter(|c| c.legacy.to_json().len() < 600)
        .take(12);
    for case in systematic {
        let mut all = Vec::new();
        nodes(&case.legacy, &mut all);
        for i in 0..all.len() {
            for t in &types {
                let mut at = i;
                let legacy = edit(&case.legacy, &mut at, &Edit::Replace(t)).unwrap_or(Val::Null);
                let mode = rng.next();
                check(
                    case,
                    &legacy,
                    &case.request,
                    case.documents.as_ref(),
                    None,
                    mode,
                    &mut failures,
                );
                ran += 1;
            }
        }
        for doc in &case.docs {
            let Ok(d) = parse(doc) else { continue };
            let mut all = Vec::new();
            nodes(&d, &mut all);
            for i in 0..all.len() {
                for t in &types {
                    let mut at = i;
                    let damaged = edit(&d, &mut at, &Edit::Replace(t)).unwrap_or(Val::Null);
                    let mode = rng.next();
                    check(
                        case,
                        &case.legacy,
                        &case.request,
                        case.documents.as_ref(),
                        Some(damaged.to_json()),
                        mode,
                        &mut failures,
                    );
                    ran += 1;
                }
            }
        }
    }
    let mut indexed: Vec<Indexed> = Vec::new();
    for case in &cases {
        let mut l = Vec::new();
        nodes(&case.legacy, &mut l);
        let mut r = Vec::new();
        nodes(&case.request, &mut r);
        let ds = case
            .docs
            .iter()
            .filter_map(|d| parse(d).ok())
            .map(|d| {
                let mut all = Vec::new();
                nodes(&d, &mut all);
                all
            })
            .collect();
        let docs = case.documents.as_ref().map(|d| {
            let mut all = Vec::new();
            nodes(d, &mut all);
            all
        });
        indexed.push(Indexed {
            legacy: l,
            request: r,
            docs: ds,
            documents: docs,
        });
    }
    let mut random = 0usize;
    while random < total {
        let k = (rng.next() as usize) % cases.len();
        let case = &cases[k];
        let Indexed {
            legacy: l_all,
            request: r_all,
            docs: d_all,
            documents: docs_all,
        } = &indexed[k];
        let mode = rng.next();
        let mut legacy = case.legacy.clone();
        let mut request = case.request.clone();
        let mut documents = case.documents.clone();
        let mut doc: Option<String> = None;
        match rng.next() % 10 {
            0..=3 => legacy = mutate(&legacy, l_all, &swaps, &mut rng),
            4..=6 if !case.docs.is_empty() => {
                let j = (rng.next() as usize) % case.docs.len();
                let d = parse(&case.docs[j]).unwrap_or(Val::Null);
                let all = d_all.get(j).map(Vec::as_slice).unwrap_or(&[]);
                let mut damaged = mutate(&d, all, &swaps, &mut rng);
                if rng.next().is_multiple_of(3) {
                    damaged = mutate(&damaged, all, &swaps, &mut rng);
                }
                doc = Some(damaged.to_json());
            }
            7 => request = mutate(&request, r_all, &swaps, &mut rng),
            8 if documents.is_some() => {
                let all = docs_all.as_deref().unwrap_or(&[]);
                documents = documents.map(|d| mutate(&d, all, &swaps, &mut rng));
            }
            _ => {
                let text = match case.docs.first() {
                    Some(d) if rng.next().is_multiple_of(2) => d.clone(),
                    _ => legacy.to_json(),
                };
                let short = cut(&text, &mut rng);
                match case.docs.first() {
                    Some(_) if text != legacy.to_json() => doc = Some(short),
                    _ => legacy = parse(&short).unwrap_or(Val::Null),
                }
            }
        }
        check(
            case,
            &legacy,
            &request,
            documents.as_ref(),
            doc,
            mode,
            &mut failures,
        );
        random += 1;
        ran += 1;
    }
    eprintln!(
        "damage fuzz: {ran} mutated inputs ({random} random), seed {seed}, convert and restore calls: {} Ok, {} Err",
        tally.ok, tally.err
    );
    assert!(
        failures.is_empty(),
        "convert or restore panicked on {} inputs, first:\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}

#[test]
fn self_referencing_paths_are_refused() {
    let request = "{}";
    for doc in [
        r#"{"version":1,"kind":"graph","nodes":[{"id":"a","type":"decision","perf":1,"sub":{"path":0,"timings":[[]]}}],"paths":[{"nodes":[{"id":"b","type":"decision","perf":1,"sub":{"path":0}}]}]}"#,
        r#"{"version":1,"kind":"graph","nodes":[{"id":"a","type":"decision","perf":1,"loop":{"path":0,"iterations":2,"timings":[[],[]]}}],"paths":[{"nodes":[{"id":"b","type":"decision","perf":1,"loop":{"path":0,"iterations":2}}]}]}"#,
    ] {
        let r = restore(RestoreArgs {
            trace: Input::Json(doc),
            request: Input::Json(request),
            documents: None,
        });
        assert!(r.is_err(), "{doc}");
    }
}

fn nested_legacy(depth: usize) -> String {
    let leaf = r#"{"in":{"id":"in","input":null,"name":"in","order":0,"output":{"x":1},"performance":"1.0µs","traceData":null}}"#;
    let mut t = leaf.to_string();
    for _ in 0..depth {
        t = format!(
            r#"{{"in":{{"id":"in","input":null,"name":"in","order":0,"output":{{"x":1}},"performance":"1.0µs","traceData":null}},"dec":{{"id":"dec","input":{{"x":1}},"name":"dec","order":1,"output":{{"x":1}},"performance":"2.0µs","traceData":{t}}}}}"#
        );
    }
    t
}

fn nested_doc(depth: usize) -> String {
    let mut sub = r#"{"nodes":[{"id":"in","type":"input","perf":1}]}"#.to_string();
    for _ in 0..depth {
        sub = format!(r#"{{"nodes":[{{"id":"dec","type":"decision","perf":2,"sub":{sub}}}]}}"#);
    }
    format!(
        r#"{{"version":1,"kind":"graph","nodes":[{{"id":"dec","type":"decision","perf":2,"sub":{sub}}}]}}"#
    )
}

#[test]
fn deeply_nested_sub_decisions_are_bounded() {
    let request = r#"{"x":1}"#;
    let ok = nested_legacy(200);
    let out = convert(ConvertArgs::new(
        LegacyTrace::Json(&ok),
        Input::Json(request),
    ))
    .unwrap();
    assert_eq!(out.status, zen_engine::trace::Status::Converted);
    let back = restore_json(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Json(request),
        documents: None,
    })
    .unwrap();
    assert_eq!(
        parse(&back).unwrap().to_json(),
        parse(&ok).unwrap().to_json()
    );
    let deep = nested_legacy(300);
    let out = convert(ConvertArgs::new(
        LegacyTrace::Json(&deep),
        Input::Json(request),
    ))
    .unwrap();
    assert!(matches!(
        out.status,
        zen_engine::trace::Status::Verbatim { .. }
    ));
    for depth in [200, 300] {
        let r = restore(RestoreArgs {
            trace: Input::Json(&nested_doc(depth)),
            request: Input::Json(request),
            documents: None,
        });
        assert_eq!(r.is_ok(), depth < 255, "{depth}");
    }
}

fn pointers(v: &Val, at: &str, out: &mut Vec<String>) {
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

fn env_count(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(default)
}

#[test]
fn redaction_with_random_pointers_errors_or_restores() {
    let singles = env_count("ZEN_TRACE_REDACT_SINGLES", 12);
    let sets = env_count("ZEN_TRACE_REDACT_SETS", 4);
    let mut rng = Rng(0x51ed_270b_2738_6ad3);
    let (mut redacted, mut refused) = (0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();
    for case in cases() {
        let error = case.error.as_ref().map(|(n, m)| (n.as_str(), m.as_deref()));
        let (own, _) = zen_engine::trace::testing::convert_val(
            &case.legacy,
            &case.request,
            error,
            Some("fuzz"),
            case.documents.as_ref(),
        );
        for doc in case.docs.iter().chain(std::iter::once(&own)) {
            if restore_val(doc, &case.request, case.documents.as_ref()).is_err() {
                continue;
            }
            let Some(plain) = zen_engine::trace::testing::unpacked(doc) else {
                continue;
            };
            let mut all = Vec::new();
            pointers(&plain, "", &mut all);
            if all.is_empty() {
                continue;
            }
            let mut picks: Vec<Vec<&str>> = Vec::new();
            match singles >= all.len() {
                true => picks.extend(all.iter().map(|p| vec![p.as_str()])),
                false => picks.extend(
                    (0..singles).map(|_| vec![all[(rng.next() as usize) % all.len()].as_str()]),
                ),
            }
            for _ in 0..sets {
                let n = 2 + (rng.next() as usize) % 4;
                picks.push(
                    (0..n)
                        .map(|_| all[(rng.next() as usize) % all.len()].as_str())
                        .collect(),
                );
            }
            for set in picks {
                let r = catch_unwind(AssertUnwindSafe(|| match redact(doc, &set, Some("fuzz")) {
                    Err(_) => Ok(false),
                    Ok(out) => restore_val(&out, &case.request, case.documents.as_ref())
                        .map(|_| true)
                        .map_err(|e| format!("{e}")),
                }));
                match r {
                    Ok(Ok(true)) => redacted += 1,
                    Ok(Ok(false)) => refused += 1,
                    Ok(Err(e)) if failures.len() < 20 => {
                        failures.push(format!("{set:?}: {e}\n  {}", &doc[..doc.len().min(300)]))
                    }
                    Err(_) if failures.len() < 20 => {
                        failures.push(format!("{set:?}: panic\n  {}", &doc[..doc.len().min(300)]))
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "redacted documents that do not restore:\n{}",
        failures.join("\n")
    );
    assert!(redacted > 1000 && refused > 1000, "{redacted} {refused}");
}

fn segments(p: &str) -> Vec<String> {
    p.split('/')
        .skip(1)
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect()
}

fn fits(pattern: &str, p: &str) -> bool {
    let segs = segments(p);
    let pat: Vec<&str> = pattern.split('/').collect();
    pat.len() == segs.len()
        && pat.iter().zip(&segs).all(|(w, s)| match *w {
            "*" => true,
            "#" => !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()),
            "@" => !s.starts_with('$') && !s.bytes().all(|b| b.is_ascii_digit()),
            w => w == s,
        })
}

const VALUE_CLASSES: [&str; 26] = [
    "nodes/#/input",
    "nodes/#/output",
    "nodes/#/output/@",
    "nodes/#/input/$set/*",
    "nodes/#/output/$set/*",
    "nodes/#/output/$each/*/$set/*/#",
    "nodes/#/results/@",
    "nodes/#/results/#/value",
    "nodes/#/results/#/@",
    "nodes/#/rule/cells/@",
    "nodes/#/rule/references/@",
    "nodes/#/rules/#/cells/@",
    "nodes/#/rulesPerItem/#/#/cells/@",
    "nodes/#/log",
    "nodes/#/sub/nodes/#/output",
    "nodes/#/loop/#/nodes/#/output",
    "steps/#/keys/*/#/value",
    "steps/#/perItem/*/#/#/value",
    "executions/#/writes/@",
    "executions/#/writes/#/value",
    "executions/#/writes/#/values/#",
    "executions/#/storedOperands/@",
    "executions/#/writes/@/#",
    "executions/#/perMember/#/writes/#/value",
    "steps/#/columns/*/#/values/#",
    "properties/set/@",
];

const STRUCTURAL: [&str; 32] = [
    "nodes/#",
    "nodes/#/id",
    "nodes/#/type",
    "nodes/#/perf",
    "nodes/#/name",
    "nodes/#/results",
    "nodes/#/results/#",
    "nodes/#/results/#/key",
    "nodes/#/rule",
    "nodes/#/rule/index",
    "nodes/#/rule/cells",
    "nodes/#/rules",
    "nodes/#/rules/#",
    "nodes/#/rulesPerItem/#",
    "nodes/#/branches",
    "nodes/#/branches/#",
    "nodes/#/scope",
    "nodes/#/items",
    "nodes/#/sub",
    "nodes/#/loop",
    "nodes/#/loop/#",
    "nodes/#/input/$merge",
    "nodes/#/output/$from",
    "nodes/#/output/$set",
    "paths/#",
    "steps/#/node",
    "executions/#",
    "executions/#/block",
    "executions/#/kind",
    "executions/#/writes",
    "executions/#/writes/#/path",
    "executions/#/arms",
];

#[test]
fn redaction_of_every_value_class_restores_the_placeholder() {
    let mut found: Vec<&str> = Vec::new();
    let mut failures = Vec::new();
    for case in cases() {
        for doc in &case.docs {
            if restore_val(doc, &case.request, case.documents.as_ref()).is_err() {
                continue;
            }
            let Some(plain) = zen_engine::trace::testing::unpacked(doc) else {
                continue;
            };
            let mut all = Vec::new();
            pointers(&plain, "", &mut all);
            for class in VALUE_CLASSES {
                if found.contains(&class) {
                    continue;
                }
                let Some(p) = all.iter().rfind(|p| fits(class, p)) else {
                    continue;
                };
                found.push(class);
                let back = redact(doc, &[p], Some("pii"))
                    .map_err(|e| e.to_string())
                    .and_then(|r| {
                        restore_val(&r, &case.request, case.documents.as_ref())
                            .map_err(|e| e.to_string())
                    });
                match back {
                    Ok(v) if v.to_json().contains("[redacted]") => {}
                    Ok(_) => failures.push(format!("{class} ({p}): no placeholder")),
                    Err(e) => failures.push(format!("{class} ({p}): {e}")),
                }
            }
        }
    }
    let missing: Vec<_> = VALUE_CLASSES
        .iter()
        .filter(|c| !found.contains(c))
        .collect();
    assert!(missing.is_empty(), "no fixture has {missing:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn redaction_of_structural_positions_is_refused() {
    let mut found: Vec<&str> = Vec::new();
    let mut failures = Vec::new();
    for case in cases() {
        for doc in &case.docs {
            if restore_val(doc, &case.request, case.documents.as_ref()).is_err() {
                continue;
            }
            let Some(plain) = zen_engine::trace::testing::unpacked(doc) else {
                continue;
            };
            let mut all = Vec::new();
            pointers(&plain, "", &mut all);
            for class in STRUCTURAL {
                for p in all.iter().filter(|p| fits(class, p)).take(3) {
                    if !found.contains(&class) {
                        found.push(class);
                    }
                    match redact(doc, &[p], None) {
                        Err(e) if e.to_string().contains("not a value position") => {}
                        Err(e) => failures.push(format!("{class} ({p}): {e}")),
                        Ok(_) => failures.push(format!("{class} ({p}): accepted")),
                    }
                }
            }
            for p in ["/nodes/99/output", "/executions/99/writes/~1x", "/nope"] {
                if redact(doc, &[p], None).is_ok() {
                    failures.push(format!("{p}: accepted a pointer that does not resolve"));
                }
            }
        }
    }
    let missing: Vec<_> = STRUCTURAL.iter().filter(|c| !found.contains(c)).collect();
    assert!(missing.is_empty(), "no fixture has {missing:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const SPLICE_DOC: &str = r#"{"version":1,"kind":"graph","nodes":[{"id":"input","type":"input","perf":1000},{"id":"c","type":"custom","perf":1000,"output":{"$splice":{"/l":[0,1,"x",{"a":1}]}},"details":{"note":"n"}},{"id":"output","type":"output","perf":1000}]}"#;

#[test]
fn redaction_inside_operators_and_verbatim_traces() {
    let request = parse(r#"{"l":[1,2]}"#).unwrap();
    let restored = |doc: &str| restore_val(doc, &request, None).map(|v| v.to_json());
    let plain = restored(SPLICE_DOC).unwrap();
    assert!(plain.contains("\"x\""), "{plain}");
    for p in [
        "/nodes/1/output/$splice/~1l/2",
        "/nodes/1/output/$splice/~1l/3",
        "/nodes/1/output/$splice/~1l/3/a",
        "/nodes/1/details",
        "/nodes/1/details/note",
    ] {
        let out = redact(SPLICE_DOC, &[p], None).unwrap_or_else(|e| panic!("{p}: {e}"));
        let back = restored(&out).unwrap_or_else(|e| panic!("{p}: {e}"));
        assert!(back.contains("[redacted]"), "{p}: {back}");
    }
    for p in [
        "/nodes/1/output/$splice",
        "/nodes/1/output/$splice/~1l",
        "/nodes/1/output/$splice/~1l/0",
        "/nodes/1/output/$splice/~1l/1",
    ] {
        assert!(redact(SPLICE_DOC, &[p], None).is_err(), "{p}");
    }
    let legacy = r#"{"input":{"id":"input","name":"input","order":0,"input":null,"output":{"a":1},"performance":"1.0µs","traceData":null},"weird":{"id":"other","order":7}}"#;
    let out = convert(ConvertArgs::new(
        LegacyTrace::Json(legacy),
        Input::Json(r#"{"a":1}"#),
    ))
    .unwrap();
    assert!(out.json.contains("\"verbatim\""), "{}", out.json);
    for p in ["/verbatim", "/verbatim/input/output"] {
        let r = redact(&out.json, &[p], Some("pii")).unwrap_or_else(|e| panic!("{p}: {e}"));
        let back = restore_json(RestoreArgs {
            trace: Input::Json(&r),
            request: Input::Json(r#"{"a":1}"#),
            documents: None,
        })
        .unwrap_or_else(|e| panic!("{p}: {e}"));
        assert!(back.contains("[redacted]"), "{p}: {back}");
    }
}

#[test]
fn markers_in_structural_positions_are_refused_by_the_reader() {
    let request = parse(r#"{"l":[1,2]}"#).unwrap();
    let marker = r#"{"$redacted":true}"#;
    let docs = [
        SPLICE_DOC.replace(r#""id":"c","#, &format!(r#""id":{marker},"#)),
        SPLICE_DOC.replace(r#""perf":1000,"output""#, &format!(r#""perf":{marker},"output""#)),
        SPLICE_DOC.replace(r#"[0,1,"x""#, &format!(r#"[{marker},1,"x""#)),
        SPLICE_DOC.replace(r#"{"$splice":{"/l":[0,1,"x",{"a":1}]}}"#, &format!(r#"{{"$splice":{{"/l":{marker}}}}}"#)),
        SPLICE_DOC.replace(r#"{"id":"output","type":"output","perf":1000}"#, marker),
        SPLICE_DOC.replace(r#""kind":"graph","#, &format!(r#""kind":"graph","seed":{marker},"#)),
        r#"{"version":1,"kind":"graph","nodes":[{"id":"t","type":"table","perf":1,"rule":{"$redacted":true}}]}"#.to_string(),
        r#"{"version":1,"kind":"graph","nodes":[{"id":"t","type":"table","perf":1,"rules":[{"$omitted":{}}]}]}"#.to_string(),
        r#"{"version":1,"kind":"graph","nodes":[{"id":"s","type":"switch","perf":1,"branches":{"$redacted":true}}]}"#.to_string(),
        r#"{"version":1,"kind":"policy","executions":[{"block":{"$redacted":true},"kind":"expression","writes":{"/a":1}}]}"#.to_string(),
        r#"{"version":1,"kind":"policy","executions":[{"block":"b","kind":"expression","writes":{"$redacted":true}}]}"#.to_string(),
    ];
    let requires = parse(r#"["redacted","truncated"]"#).unwrap();
    for doc in docs {
        let mut v = parse(&doc).unwrap();
        if let Val::Obj(o) = &v {
            let mut o = (**o).clone();
            o.set("requires", requires.clone());
            v = Val::obj(o);
        }
        let doc = v.to_json();
        match restore_val(&doc, &request, None) {
            Err(e) => assert!(e.to_string().contains("structural"), "{doc}: {e}"),
            Ok(_) => panic!("accepted {doc}"),
        }
    }
    let keyed = r#"{"version":1,"kind":"graph","requires":["redacted"],"nodes":[{"id":"input","type":"input","perf":1},{"id":"e","type":"expression","perf":1,"results":{"$redacted":true}}]}"#;
    let back = restore_val(keyed, &request, None).unwrap().to_json();
    assert!(back.contains(r#""$redacted":{"result":true}"#), "{back}");
    let ok = SPLICE_DOC
        .replace(r#""x""#, r#"{"$redacted":true,"reason":"pii"}"#)
        .replace(
            r#""kind":"graph","#,
            r#""kind":"graph","requires":["redacted"],"#,
        );
    let back = restore_val(&ok, &request, None).unwrap().to_json();
    assert!(back.contains("[redacted]"), "{back}");
}

use std::io::Write;
use zen_engine::trace::testing::{convert_val, first_diff, parse, restore_val, Val};
use zen_engine::trace::Status;

struct Fixture {
    id: String,
    request: Val,
    error: Option<(String, Option<String>)>,
    documents: Option<Val>,
    trace: Val,
    poc: String,
    poc_no_docs: Option<String>,
}

fn load() -> Vec<Fixture> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/trace/poc.jsonl"
    );
    let text = std::fs::read_to_string(path).expect("fixtures");
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let v = parse(l).expect("fixture json");
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
            let error = v.get("error").filter(|e| e.is_obj()).map(|e| {
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
            Fixture {
                id: s("id").unwrap(),
                request: v.get("request").cloned().unwrap_or(Val::Null),
                error,
                documents: v.get("documents").cloned(),
                trace: v.get("trace").cloned().unwrap(),
                poc: s("poc").unwrap(),
                poc_no_docs: s("pocNoDocs"),
            }
        })
        .collect()
}

#[test]
fn fixtures_round_trip_and_match_the_poc() {
    let fixtures = load();
    let out_path = std::env::var("ZEN_TRACE_RUST_OUT").ok();
    let mut out = out_path
        .as_ref()
        .map(|p| std::fs::File::create(p).expect("out file"));
    let mut failures: Vec<String> = Vec::new();
    let (mut rust_bytes, mut poc_bytes, mut identical) = (0usize, 0usize, 0usize);
    let (mut rust_nd, mut poc_nd, mut identical_nd, mut policies) =
        (0usize, 0usize, 0usize, 0usize);
    for f in &fixtures {
        let err = f.error.as_ref().map(|(n, m)| (n.as_str(), m.as_deref()));
        let (text, status) = convert_val(&f.trace, &f.request, err, None, f.documents.as_ref());
        if status != Status::Converted {
            failures.push(format!("{}: verbatim {:?}", f.id, status));
        }
        match restore_val(&text, &f.request, f.documents.as_ref()) {
            Ok(back) => {
                if let Some(d) = first_diff(&f.trace, &back) {
                    failures.push(format!("{}: rust restore differs at {d}", f.id));
                }
            }
            Err(e) => failures.push(format!("{}: rust restore failed: {e}", f.id)),
        }
        match restore_val(&f.poc, &f.request, f.documents.as_ref()) {
            Ok(back) => {
                if let Some(d) = first_diff(&f.trace, &back) {
                    failures.push(format!("{}: restore of poc text differs at {d}", f.id));
                }
            }
            Err(e) => failures.push(format!("{}: restore of poc text failed: {e}", f.id)),
        }
        rust_bytes += text.len();
        poc_bytes += f.poc.len();
        if text == f.poc {
            identical += 1;
        } else if std::env::var("ZEN_TRACE_DIFF").is_ok() {
            let at = text
                .bytes()
                .zip(f.poc.bytes())
                .position(|(a, b)| a != b)
                .unwrap_or(text.len().min(f.poc.len()));
            let lo = at.saturating_sub(150);
            eprintln!(
                "{} differs at byte {at}:\n  rust {}\n  poc  {}",
                f.id,
                &text[lo..(at + 200).min(text.len())],
                &f.poc[lo..(at + 200).min(f.poc.len())]
            );
        }
        let mut no_docs = None;
        if let Some(pnd) = &f.poc_no_docs {
            policies += 1;
            let (t2, s2) = convert_val(&f.trace, &f.request, err, None, None);
            if s2 != Status::Converted {
                failures.push(format!("{}: without documents verbatim {:?}", f.id, s2));
            }
            match restore_val(&t2, &f.request, None) {
                Ok(back) => {
                    if let Some(d) = first_diff(&f.trace, &back) {
                        failures.push(format!(
                            "{}: rust restore without documents differs at {d}",
                            f.id
                        ));
                    }
                }
                Err(e) => failures.push(format!(
                    "{}: rust restore without documents failed: {e}",
                    f.id
                )),
            }
            match restore_val(pnd, &f.request, None) {
                Ok(back) => {
                    if let Some(d) = first_diff(&f.trace, &back) {
                        failures.push(format!(
                            "{}: restore of poc text without documents differs at {d}",
                            f.id
                        ));
                    }
                }
                Err(e) => failures.push(format!(
                    "{}: restore of poc text without documents failed: {e}",
                    f.id
                )),
            }
            rust_nd += t2.len();
            poc_nd += pnd.len();
            if t2 == *pnd {
                identical_nd += 1;
            }
            no_docs = Some(t2);
        }
        if let Some(o) = out.as_mut() {
            let mut line = format!(
                "{{\"id\":{},\"v1\":{}",
                Val::str(&f.id).to_json(),
                Val::str(&text).to_json()
            );
            if let Some(nd) = &no_docs {
                line.push_str(&format!(",\"v1NoDocs\":{}", Val::str(nd).to_json()));
            }
            line.push('}');
            writeln!(o, "{line}").unwrap();
        }
    }
    eprintln!(
        "fixtures: {} traces, {} byte-identical to the POC, rust {} B vs poc {} B ({:+.2}%)",
        fixtures.len(),
        identical,
        rust_bytes,
        poc_bytes,
        (rust_bytes as f64 / poc_bytes as f64 - 1.0) * 100.0
    );
    eprintln!(
        "policies without documents: {} traces, {} byte-identical, rust {} B vs poc {} B ({:+.2}%)",
        policies,
        identical_nd,
        rust_nd,
        poc_nd,
        (rust_nd as f64 / poc_nd.max(1) as f64 - 1.0) * 100.0
    );
    for f in failures.iter().take(40) {
        eprintln!("  {f}");
    }
    assert!(failures.is_empty(), "{} failures", failures.len());
    let ratio = rust_bytes as f64 / poc_bytes as f64;
    assert!((0.97..=1.03).contains(&ratio), "size ratio {ratio}");
}

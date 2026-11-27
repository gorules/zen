use zen_engine::trace::testing::{eq, first_diff, parse, restore_val, Val};

fn cases() -> Vec<Val> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/trace/additions.jsonl"
    );
    std::fs::read_to_string(path)
        .expect("fixtures")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| parse(l).expect("fixture line"))
        .collect()
}

fn text(v: Option<&Val>) -> Option<String> {
    v.and_then(|x| x.as_str()).map(str::to_string)
}

#[test]
fn additions_restore_like_the_poc() {
    let mut failures = Vec::new();
    let all = cases();
    for case in &all {
        let name = text(case.get("name")).unwrap_or_default();
        let doc = case.get("doc").expect("doc").to_json();
        let request = case.get("request").cloned().unwrap_or(Val::Null);
        let back = restore_val(&doc, &request, case.get("documents"));
        match (case.get("legacy"), back) {
            (Some(want), Ok(got)) if eq(want, &got) => {}
            (Some(want), Ok(got)) => failures.push(format!(
                "{name}: differs at {}",
                first_diff(want, &got).unwrap_or_default()
            )),
            (Some(_), Err(e)) => failures.push(format!("{name}: refused: {e}")),
            (None, Ok(_)) => failures.push(format!("{name}: should be refused")),
            (None, Err(_)) => {}
        }
        if let Some(want) = case.get("inspect") {
            match zen_engine::trace::inspect(&doc) {
                Ok(i) => {
                    let checks = [
                        ("kind", Some(i.kind.clone())),
                        ("engine", i.engine.clone()),
                        ("evaluatedAt", i.evaluated_at.clone()),
                        ("timezone", i.timezone.clone()),
                    ];
                    for (k, got) in checks {
                        if text(want.get(k)) != got {
                            failures.push(format!("{name}: inspect {k}"));
                        }
                    }
                    let seed = want
                        .get("seed")
                        .and_then(|s| s.as_usize())
                        .map(|s| s as u64);
                    if seed != i.seed {
                        failures.push(format!("{name}: inspect seed"));
                    }
                    if matches!(want.get("truncated"), Some(Val::Bool(true))) != i.truncated {
                        failures.push(format!("{name}: inspect truncated"));
                    }
                    if want.get("context").is_some_and(|c| !c.is_null()) != i.context.is_some() {
                        failures.push(format!("{name}: inspect context"));
                    }
                }
                Err(e) => failures.push(format!("{name}: inspect refused: {e}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(all.len() >= 29);
}

#[test]
fn redaction_by_pointer_restores_the_placeholder_there_only() {
    for case in cases() {
        let Some(want) = case.get("legacy") else {
            continue;
        };
        if text(case.get("name")).as_deref() != Some("rows/single-run") {
            continue;
        }
        let doc = case.get("doc").expect("doc").to_json();
        let request = case.get("request").cloned().unwrap_or(Val::Null);
        let redacted = zen_engine::trace::redact(&doc, &["/nodes/1/results/0/value"], Some("pii"))
            .expect("redact");
        assert!(redacted.contains("\"$redacted\":true"));
        assert!(redacted.contains("\"redacted\""));
        let back = restore_val(&redacted, &request, None).expect("restore");
        assert!(
            eq(want, &back),
            "the first row is overwritten by the second"
        );
        let last =
            zen_engine::trace::redact(&doc, &["/nodes/1/results/1/value"], None).expect("redact");
        let back = restore_val(&last, &request, None).expect("restore");
        assert!(!eq(want, &back));
        assert!(back.to_json().contains("[redacted]"));
        assert!(zen_engine::trace::redact(&doc, &["/requires"], None).is_err());
        assert!(zen_engine::trace::redact(&doc, &["/nodes/9"], None).is_err());
        return;
    }
    panic!("rows/single-run fixture missing");
}

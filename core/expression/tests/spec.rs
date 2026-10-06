mod conformance;

use conformance::{Note, Spec};
use std::collections::BTreeMap;

#[derive(Default)]
struct Tally {
    total: usize,
    failed: usize,
    bugs: usize,
    questions: usize,
}

#[test]
fn spec_matches_stack_vm() {
    Spec::prepare();
    let (cases, problems) = Spec::cases();
    let mut failures = Vec::new();
    let mut bugs = Vec::new();
    let mut files: BTreeMap<String, Tally> = BTreeMap::new();
    for case in &cases {
        let tally = files.entry(case.file.clone()).or_default();
        tally.total += 1;
        let verdict = case.verdict(&case.stack());
        match (&case.note, verdict) {
            (Note::Bug(note), Some(failure)) => {
                tally.bugs += 1;
                bugs.push(format!("{failure} | bug: {note}"));
            }
            (Note::Bug(note), None) => {
                tally.failed += 1;
                failures.push(format!(
                    "{} | {} | passes now, drop the bug note ({note})",
                    case.location(),
                    case.expression
                ));
            }
            (note, Some(failure)) => {
                tally.failed += 1;
                tally.questions += matches!(note, Note::Question(_)) as usize;
                failures.push(failure);
            }
            (note, None) => tally.questions += matches!(note, Note::Question(_)) as usize,
        }
    }
    for (file, t) in &files {
        eprintln!(
            "{file}: {} cases, {} failed, {} known bugs, {} questions",
            t.total, t.failed, t.bugs, t.questions
        );
    }
    eprintln!(
        "spec: {} cases, {} failed, {} known bugs, {} malformed",
        cases.len(),
        failures.len(),
        bugs.len(),
        problems.len()
    );
    if std::env::var("SPEC_BUGS").is_ok() {
        eprintln!("known bugs:\n{}", bugs.join("\n"));
    }
    let limit: usize = std::env::var("SPEC_SHOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    assert!(
        problems.is_empty() && failures.is_empty(),
        "malformed:\n{}\nfailures:\n{}",
        problems.join("\n"),
        failures
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

mod conformance;

use conformance::Spec;

#[test]
fn isolate_matches_on_both_engines_when_lane_is_enabled() {
    Spec::prepare();
    let (cases, problems) = Spec::cases();
    assert!(
        problems.is_empty(),
        "malformed spec:\n{}",
        problems.join("\n")
    );
    let stack: Vec<bool> = cases
        .iter()
        .map(|case| case.verdict(&case.exact()).is_none())
        .collect();
    let lane: Vec<bool> = cases
        .iter()
        .map(|case| case.verdict(&case.laned()).is_none())
        .collect();
    let failures: Vec<String> = cases
        .iter()
        .zip(stack.iter().zip(&lane))
        .filter(|(_, (a, b))| a != b)
        .take(40)
        .map(|(case, (a, b))| format!("{} | stack passes {a} | lane passes {b}", case.location()))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Spec::no_fallbacks();
}

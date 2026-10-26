mod conformance;

use conformance::Spec;
use std::sync::atomic::Ordering;
use zen_expression::ZEN_EXPRESSION_CONFIG;

#[test]
fn isolate_matches_on_both_engines_when_lane_is_enabled() {
    Spec::prepare();
    let (cases, problems) = Spec::cases();
    assert!(
        problems.is_empty(),
        "malformed spec:\n{}",
        problems.join("\n")
    );
    let run = || -> Vec<bool> {
        cases
            .iter()
            .map(|case| case.verdict(&case.exact()).is_none())
            .collect()
    };
    let stack = run();
    ZEN_EXPRESSION_CONFIG.lane.store(true, Ordering::Relaxed);
    let lane = run();
    ZEN_EXPRESSION_CONFIG.lane.store(false, Ordering::Relaxed);
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

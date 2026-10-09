use serde_json::json;
use zen_expression::Isolate;

fn run(expression: &str, env: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let mut isolate = Isolate::new();
    isolate.set_environment(env.into());
    Ok(isolate.run_standard(expression)?.to_value())
}

#[test]
fn percentile_approx_fraction_out_of_range_fails() {
    let env = json!({ "items": [{ "v": 1 }, { "v": 2 }] });
    assert!(run("percentileApprox(items, #.v, 1.5)", env.clone()).is_err());
    assert!(run("percentileApprox(items, #.v, -0.1)", env.clone()).is_err());
    assert!(run("percentileApprox(items, #.v, 1)", env).is_ok());
}

#[test]
fn numeric_aggregates_reject_other_values() {
    let env = json!({ "items": [{ "v": 1 }, { "v": "a" }] });
    assert!(run("skew(items, #.v)", env.clone()).is_err());
    assert!(run("kurtosis(items, #.v)", env.clone()).is_err());
    assert!(run("percentileApprox(items, #.v, 0.5)", env).is_err());
}

#[test]
fn arg_max_ranks_numbers_or_dates_not_both() {
    let env = json!({ "items": [{ "m": "A", "a": 1 }, { "m": "B", "a": "x" }] });
    assert!(run("argMax(items, #.m, #.a)", env).is_err());
    let env = json!({ "items": [{ "m": "A", "a": 1, "at": "2024-01-01" }, { "m": "B", "at": "2024-01-02" }] });
    assert!(run("argMax(items, #.m, #.a ?? d(#.at))", env).is_err());
}

#[test]
fn skew_overflow_is_null() {
    let env = json!({ "items": [{ "v": "79228162514264337593543950335" }, { "v": "1" }] });
    let result = run("skew(items, number(#.v))", env.clone()).unwrap();
    assert_eq!(result, serde_json::Value::Null);
    let result = run("kurtosis(items, number(#.v))", env).unwrap();
    assert_eq!(result, serde_json::Value::Null);
}

#[test]
fn approximations_at_volume() {
    let items: Vec<_> = (0..20_000)
        .map(|i| json!({ "id": format!("c{}", i % 5_000), "v": i % 1_000 }))
        .collect();
    let env = json!({ "items": items });

    let distinct = run("countDistinctApprox(items, #.id)", env.clone()).unwrap();
    let distinct = distinct.as_f64().unwrap();
    assert!((distinct - 5_000.0).abs() / 5_000.0 <= 0.03, "{distinct}");

    // The 0.9 quantile of 0..999 repeated: 899 (the ⌊q·(n−1)⌋-th value).
    let check = run(
        "abs(percentileApprox(items, #.v, 0.9) - 899) / 899 <= 0.01",
        env.clone(),
    );
    assert_eq!(check.unwrap(), json!(true));
    let check = run("percentileApprox(items, #.v, 0)", env).unwrap();
    assert_eq!(check, json!(0));
}

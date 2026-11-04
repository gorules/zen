use serde_json::{json, Value};
use zen_expression::clock::{row_seed, unrowed_draws, Clock};
use zen_expression::lane::{LaneProgram, LaneRunner};
use zen_expression::{ExpressionKind, Scope, Variable};

fn scope(n: usize) -> Scope {
    let xs: Vec<usize> = (0..n).collect();
    Scope::new(Variable::from(json!({"xs": xs, "n": n})))
}

fn single(program: &LaneProgram, n: usize, seed: u64) -> Value {
    let clock = Clock::new(None, None, Some(seed));
    let value = clock
        .scope(|| LaneRunner::new().evaluate_one(program, &scope(n)))
        .unwrap();
    serde_json::to_value(value).unwrap()
}

fn compile(source: &str) -> LaneProgram {
    LaneProgram::compile(source, ExpressionKind::Standard).unwrap()
}

#[test]
fn items_of_a_closure_draw_independently() {
    let program = compile("map(xs, rand(1000000))");
    let value = single(&program, 40, 3);
    let mut items: Vec<String> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.to_string())
        .collect();
    items.sort();
    items.dedup();
    assert!(items.len() > 30, "{value}");
    assert_eq!(single(&program, 40, 3), value);
    assert_ne!(single(&program, 40, 4), value);
}

#[test]
fn constant_arguments_draw_per_call() {
    let program = compile("[rand(1000000), rand(1000000), map([1, 2, 3], rand(1000000))]");
    let value = single(&program, 0, 9);
    let flat: Vec<String> = value
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|v| match v {
            Value::Array(items) => items.clone(),
            other => vec![other.clone()],
        })
        .map(|v| v.to_string())
        .collect();
    let mut distinct = flat.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), flat.len(), "{value}");
}

#[test]
fn batched_rows_match_single_rows() {
    for source in [
        "rand(1000000) + n",
        "map(xs, rand(1000) * 1000 + rand(1000))",
        "filter(xs, rand(10) > 4)",
        "some(xs, rand(100) > 90)",
        "map(xs, map([1, 2], rand(100)))",
    ] {
        let program = compile(source);
        let sizes: Vec<usize> = (0..150).map(|row| row * 7 % 97).collect();
        let scopes: Vec<Scope> = sizes.iter().map(|&n| scope(n)).collect();
        let batch = Clock::new(None, None, Some(77)).batch();
        let results = batch.scope(|| LaneRunner::new().evaluate(&program, &scopes));
        for (row, result) in results.into_iter().enumerate() {
            let got = serde_json::to_value(result.unwrap()).unwrap();
            assert_eq!(
                got,
                single(&program, sizes[row], row_seed(77, row)),
                "{source}: row {row}"
            );
        }
    }
    assert_eq!(unrowed_draws(), 0);
}

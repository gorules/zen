use serde_json::{json, Value};
use zen_expression::context::{row_seed, unrowed_draws, EvaluationContext, Pinned};
use zen_expression::lane::{LaneProgram, LaneRunner};
use zen_expression::{ExpressionKind, Scope, Variable};

fn scope(n: usize) -> Scope {
    let xs: Vec<usize> = (0..n).collect();
    Scope::new(Variable::from(json!({"xs": xs, "n": n})))
}

fn single(program: &LaneProgram, n: usize, seed: u64) -> Value {
    let context = EvaluationContext::new(Pinned {
        seed: Some(seed),
        ..Pinned::default()
    });
    let value = LaneRunner::new()
        .evaluate_one(program, &context, &scope(n))
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
        let batch = EvaluationContext::batch(Pinned {
            seed: Some(77),
            ..Pinned::default()
        });
        let results = LaneRunner::new().evaluate(&program, &batch, &scopes);
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

#[test]
fn interleaved_contexts_keep_their_own_clock_and_stream() {
    use zen_expression::context::parse_instant;
    let program = compile("[rand(1000000), d().timestamp(), d().offsetName()]");
    let pinned = |now: &str, zone: &str, seed: u64| {
        EvaluationContext::new(Pinned {
            now: parse_instant(now),
            timezone: zen_expression::context::parse_timezone(zone),
            seed: Some(seed),
            locale: None,
        })
    };
    let pins = [
        ("2026-11-04T09:30:00.125Z", "Europe/Berlin", 1),
        ("2031-02-17T23:15:42.500Z", "America/New_York", 2),
    ];
    let mut runner = LaneRunner::new();
    let alone: Vec<Vec<Value>> = pins
        .iter()
        .map(|&(now, zone, seed)| {
            let context = pinned(now, zone, seed);
            (0..4)
                .map(|_| {
                    let value = runner.evaluate_one(&program, &context, &scope(0)).unwrap();
                    serde_json::to_value(value).unwrap()
                })
                .collect()
        })
        .collect();
    let contexts: Vec<EvaluationContext> = pins
        .iter()
        .map(|&(now, zone, seed)| pinned(now, zone, seed))
        .collect();
    let mut seen: Vec<Vec<Value>> = vec![Vec::new(); contexts.len()];
    for _ in 0..4 {
        for (which, context) in contexts.iter().enumerate() {
            let value = runner.evaluate_one(&program, context, &scope(0)).unwrap();
            seen[which].push(serde_json::to_value(value).unwrap());
        }
    }
    assert_eq!(seen, alone);
    assert_ne!(alone[0][0][0], alone[1][0][0]);
    assert_eq!(alone[1][0][2], json!("America/New_York"));
}

fn seeded(seed: u64) -> EvaluationContext {
    EvaluationContext::new(Pinned {
        seed: Some(seed),
        ..Pinned::default()
    })
}

fn run(program: &LaneProgram, context: &EvaluationContext) -> Value {
    let value = LaneRunner::new()
        .evaluate_one(program, context, &scope(4))
        .unwrap();
    serde_json::to_value(value).unwrap()
}

#[test]
fn draws_are_addressed_by_their_site() {
    let first = compile("[rand(1000000), rand(1000000)]");
    let pair = run(&first, &seeded(21));
    assert_ne!(pair[0], pair[1]);
    let skipped = compile("n > 100 ? rand(5) : rand(1000000)");
    assert_eq!(run(&skipped, &seeded(21)), pair[1]);
    let alone = compile("rand(1000000)");
    assert_eq!(run(&alone, &seeded(21)), pair[0]);
    let nested = compile("rand(rand(1) + 1000000)");
    assert_eq!(run(&nested, &seeded(21)), run(&nested, &seeded(21)));
}

#[test]
fn located_programs_draw_independently_of_each_other() {
    use zen_expression::context::location;
    let a = location(&["expression", "node", "a"]);
    let b = location(&["expression", "node", "b"]);
    let left = LaneProgram::located("rand(1000000)", ExpressionKind::Standard, a).unwrap();
    let right = LaneProgram::located("rand(1000000)", ExpressionKind::Standard, b).unwrap();
    let context = seeded(5);
    let alone = run(&left, &context);
    let shared = seeded(5);
    let other = run(&right, &shared);
    assert_eq!(run(&left, &shared), alone);
    assert_ne!(other, alone);
    let runtime = seeded(5).located(a);
    assert_eq!(run(&compile("rand(1000000)"), &runtime), alone);
    let many = LaneProgram::compile_many_located(
        &[("b", "rand(1000000)"), ("a", "rand(1000000)")],
        true,
        &[b, a],
    )
    .unwrap();
    let mut both = Vec::new();
    LaneRunner::new().evaluate_many(&many, &seeded(5), &[scope(4)], None, |_, values| {
        both = values.unwrap().unwrap();
    });
    let both: Vec<Value> = both
        .into_iter()
        .map(|v| serde_json::to_value(v).unwrap())
        .collect();
    assert_eq!(both, vec![other, alone]);
}

#[test]
fn nested_scopes_draw_differently() {
    use zen_expression::context::location;
    let program = compile("rand(1000000)");
    let root = run(&program, &seeded(8));
    let one = run(
        &program,
        &seeded(8).nested(location(&["decision", "one", "child"])),
    );
    let two = run(
        &program,
        &seeded(8).nested(location(&["decision", "two", "child"])),
    );
    assert_ne!(one, two);
    assert_ne!(root, one);
    assert_eq!(
        one,
        run(
            &program,
            &seeded(8).nested(location(&["decision", "one", "child"]))
        )
    );
}

#[test]
fn sessions_locate_each_evaluation() {
    use zen_expression::context::location;
    use zen_expression::lane::Session;
    let mut session = Session::new().with_context(seeded(3));
    session.locate(location(&["x"]));
    let x = session.run_standard("rand(1000000)").unwrap();
    session.locate(location(&["y"]));
    let y = session.run_standard("rand(1000000)").unwrap();
    let mut alone = Session::new().with_context(seeded(3));
    alone.locate(location(&["y"]));
    assert_eq!(alone.run_standard("rand(1000000)").unwrap(), y);
    assert_ne!(x, y);
}

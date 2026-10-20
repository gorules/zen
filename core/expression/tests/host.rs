use rust_decimal::Decimal;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::functions::{
    register_host_function, Arguments, FunctionSignature, HostFunction, StaticFunction,
};
use zen_expression::intellisense::IntelliSense;
use zen_expression::variable::{Variable, VariableType};
use zen_expression::Isolate;

fn register_double() {
    register_host_function(HostFunction {
        name: Arc::from("double"),
        description: "Doubles a number".to_string(),
        definition: Arc::new(|| {
            Rc::new(StaticFunction {
                signature: FunctionSignature::single(VariableType::Number, VariableType::Number),
                implementation: Rc::new(|args: Arguments| {
                    Ok(Variable::Number(args.number(0)? * Decimal::from(2)))
                }),
            })
        }),
    })
    .unwrap();
}

#[test]
fn host_functions_run_on_every_thread() {
    register_double();

    let mut isolate = Isolate::new();
    assert_eq!(
        isolate.run_standard("double(21) + 1").unwrap(),
        Variable::Number(Decimal::from(43))
    );

    let other = std::thread::spawn(|| {
        Isolate::new()
            .run_standard("double(2)")
            .unwrap()
            .to_value()
    })
    .join()
    .unwrap();
    assert_eq!(other, serde_json::json!(4));
}

#[test]
fn host_functions_are_type_checked_and_completed() {
    register_double();
    let mut intellisense = IntelliSense::new();

    let analysis = intellisense.analyze("double(2)", &VariableType::Any);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
    assert_eq!(analysis.return_type, VariableType::Number);

    let analysis = intellisense.analyze("double('x')", &VariableType::Any);
    assert!(!analysis.diagnostics.is_empty());

    let completions = intellisense.completions("dou", 3, &VariableType::Any);
    let double = completions
        .iter()
        .find(|c| c.label == "double")
        .expect("double completes");
    assert_eq!(double.info, "Doubles a number");
}

#[test]
fn built_in_names_cannot_be_taken_and_unknown_names_stay_unknown() {
    let taken = register_host_function(HostFunction {
        name: Arc::from("countDistinct"),
        description: String::new(),
        definition: Arc::new(|| {
            Rc::new(StaticFunction {
                signature: FunctionSignature::single(VariableType::Any, VariableType::Any),
                implementation: Rc::new(|_: Arguments| Ok(Variable::Null)),
            })
        }),
    });
    assert!(taken.is_err());

    assert!(Isolate::new().run_standard("notRegistered(1)").is_err());
}

#[test]
fn a_parse_only_host_function_fails_when_evaluated() {
    register_host_function(HostFunction {
        name: Arc::from("windowed"),
        description: "Compiled by the host, never evaluated here".to_string(),
        definition: Arc::new(|| {
            Rc::new(StaticFunction {
                signature: FunctionSignature::single(VariableType::Any, VariableType::Number),
                implementation: Rc::new(|_: Arguments| {
                    anyhow::bail!("`windowed` is evaluated by the host")
                }),
            })
        }),
    })
    .unwrap();

    let analysis = IntelliSense::new().analyze("windowed(items) > 1", &VariableType::Any);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);

    let err = Isolate::new()
        .run_standard("windowed([1])")
        .unwrap_err()
        .to_string();
    assert!(err.contains("evaluated by the host"), "{err}");
}

#[test]
fn host_functions_run_in_the_lane_vm() {
    register_double();

    let expression = zen_expression::BatchExpression::standard("double(x) + count_distinct(items)").unwrap();
    assert!(expression.is_lane());
    let inputs: Vec<Variable> = (0..100)
        .map(|i| serde_json::json!({ "x": i, "items": [1, 1, 2, null] }).into())
        .collect();
    let results = expression.evaluate(&inputs);
    for (i, result) in results.into_iter().enumerate() {
        assert_eq!(result.unwrap().to_value(), serde_json::json!(i * 2 + 2));
    }

    let program = zen_expression::lane::LaneProgram::standard("double('x')").unwrap();
    let err = zen_expression::lane::LaneRunner::new()
        .evaluate_one(&program, &zen_expression::Scope::default())
        .unwrap_err();
    let expected = Isolate::new().run_standard("double('x')").unwrap_err();
    assert_eq!(err.to_string(), expected.to_string());
}

#[test]
fn variadic_host_functions_feed_aggregates() {
    use zen_expression::functions::CompositeFunction;
    register_host_function(HostFunction {
        name: Arc::from("window_rows"),
        description: "Rows sharing the keys within a window".to_string(),
        definition: Arc::new(|| {
            Rc::new(CompositeFunction {
                signatures: (1..=4)
                    .map(|n| FunctionSignature {
                        parameters: vec![VariableType::Any; n],
                        return_type: VariableType::Any.array(),
                    })
                    .collect(),
                implementation: Rc::new(|args: Arguments| {
                    let n = args.0.len();
                    let window = args.str(n - 1)?;
                    let size = if window == "7d" { 3 } else { 1 };
                    Ok(Variable::from(serde_json::json!(
                        (0..size).map(|i| serde_json::json!({ "amount": i * 10 + n, "kind": if i == 0 { serde_json::Value::Null } else { "in".into() } })).collect::<Vec<_>>()
                    )))
                }),
            })
        }),
    })
    .unwrap();

    let source = "sum(window_rows(card, merchant, '7d') as t, t.amount) + count(window_rows(card, '1d') as t, t.kind == null) + count_distinct(window_rows(card, '7d') as t, t.kind)";
    let expected = Isolate::with_environment(serde_json::json!({ "card": "c1", "merchant": "m1" }).into())
        .run_standard(source)
        .unwrap();
    assert_eq!(expected.to_value(), serde_json::json!(3 + 13 + 23 + 1 + 1));

    let analysis = IntelliSense::new().analyze(source, &VariableType::Any);
    assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);

    let inputs: Vec<Variable> = (0..70)
        .map(|_| serde_json::json!({ "card": "c1", "merchant": "m1" }).into())
        .collect();
    for result in zen_expression::BatchExpression::standard(source).unwrap().evaluate(&inputs) {
        assert_eq!(result.unwrap(), expected);
    }
}

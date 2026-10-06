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

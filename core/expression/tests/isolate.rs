use bumpalo::Bump;
use rust_decimal_macros::dec;
use serde_json::{json, Value};

use zen_expression::isolate::Isolate;
use zen_expression::opcodes::{ExecResult, Variable};

struct TestEnv {
    env: Value,
    cases: Vec<TestCase>,
}

struct TestCase {
    expr: &'static str,
    result: ExecResult,
}

#[test]
fn isolate_standard_test() {
    let tests = Vec::from([
        TestEnv {
            env: json!({
                "hello": "Hello, ",
                "world": "world!"
            }),
            cases: Vec::from([TestCase {
                expr: "hello + world",
                result: ExecResult::String("Hello, world!".to_string()),
            }]),
        },
        TestEnv {
            env: json!({
                "a": 3,
                "b": 6,
                "c": 1
            }),
            cases: Vec::from([
                TestCase {
                    expr: "a + b - c",
                    result: ExecResult::Number(dec!(8.0)),
                },
                TestCase {
                    expr: "b^a",
                    result: ExecResult::Number(dec!(216.0)),
                },
                TestCase {
                    expr: "c * b / a",
                    result: ExecResult::Number(dec!(2.0)),
                },
                TestCase {
                    expr: "abs(a - b - c)",
                    result: ExecResult::Number(dec!(4.0)),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "a": 3,
                "b": 6,
                "c": 1,
                "t": true,
                "f": false,
            }),
            cases: Vec::from([
                TestCase {
                    expr: "a == a and a != b",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "b - a > c",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "b < a or a > b",
                    result: ExecResult::Bool(false),
                },
                TestCase {
                    expr: "t or f",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "t and f",
                    result: ExecResult::Bool(false),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: "1 in [1..5] and 5 in [1..5]",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "1 not in (1..5] and 5 not in [1..5)",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "1 not in [1.01..5] and 5 not in [1..4.99]",
                    result: ExecResult::Bool(true),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"date("2022-04-04") > date("2022-03-04")"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"duration("60m") == duration("1h")"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"duration("24h") >= duration("1d")"#,
                    result: ExecResult::Bool(true),
                },
            ]),
        },
        TestEnv {
            env: json!({ "customer": { "firstName": "John", "lastName": "Doe" } }),
            cases: Vec::from([
                TestCase {
                    expr: r#"customer.firstName + " " + customer.lastName"#,
                    result: ExecResult::String("John Doe".to_string()),
                },
                TestCase {
                    expr: r#"startsWith(customer.firstName, "Jo")"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"endsWith(customer.firstName + customer.lastName, "oe")"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"contains(customer.lastName, "Do")"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "upper(customer.firstName) == 'JOHN'",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "lower(customer.firstName) == 'john'",
                    result: ExecResult::Bool(true),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "customer": {
                    "groups": ["admin", "user"],
                    "purchaseAmounts": [100, 200, 400, 800]
                }
            }),
            cases: Vec::from([
                TestCase {
                    expr: r#"some(customer.groups, # == "admin")"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"all(customer.purchaseAmounts, # in [100..800])"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"not all(customer.purchaseAmounts, # in (100..800))"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"none(customer.purchaseAmounts, # == 99)"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"all(customer.groups, # == "admin" or # == "user")"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "count(customer.groups, true)",
                    result: ExecResult::Number(dec!(2.0)),
                },
                TestCase {
                    expr: "count(customer.purchaseAmounts, # > 150)",
                    result: ExecResult::Number(dec!(3.0)),
                },
                TestCase {
                    expr: "map(customer.purchaseAmounts, # + 50)[0] == 150",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "filter(customer.purchaseAmounts, # >= 200)[0] == 200",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "sum(customer.purchaseAmounts[0:1]) == 300",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "one(customer.groups, # == 'admin')",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "one(['admin', 'admin'], # == 'admin')",
                    result: ExecResult::Bool(false),
                },
                TestCase {
                    expr: r#"map(["admin", "user"], "hello " + #)"#,
                    result: ExecResult::Array(Vec::from([
                        ExecResult::String("hello admin".to_string()),
                        ExecResult::String("hello user".to_string()),
                    ])),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "name": "Hello",
                "groups": ["admin", "user"],
                "purchaseAmounts": [100, 200, 400, 800]
            }),
            cases: Vec::from([
                TestCase {
                    expr: r#"contains(name, 'ello')"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"contains(name, '123')"#,
                    result: ExecResult::Bool(false),
                },
                TestCase {
                    expr: r#"contains(groups, 'admin')"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"contains(groups, 'hello')"#,
                    result: ExecResult::Bool(false),
                },
                TestCase {
                    expr: r#"contains(purchaseAmounts, 100)"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "contains(purchaseAmounts, 150)",
                    result: ExecResult::Bool(false),
                },
                TestCase {
                    expr: "len(purchaseAmounts)",
                    result: ExecResult::Number(dec!(4.0)),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"dayOfWeek(date("2022-11-08"))"#,
                    result: ExecResult::Number(dec!(2.0)),
                },
                TestCase {
                    expr: r#"dayOfMonth(date("2022-11-09"))"#,
                    result: ExecResult::Number(dec!(9.0)),
                },
                TestCase {
                    expr: r#"dayOfYear(date("2022-11-10"))"#,
                    result: ExecResult::Number(dec!(314.0)),
                },
                TestCase {
                    expr: r#"weekOfYear(date("2022-11-12"))"#,
                    result: ExecResult::Number(dec!(45.0)),
                },
                TestCase {
                    expr: r#"monthString(date("2022-11-14"))"#,
                    result: ExecResult::String("Nov".to_string()),
                },
                TestCase {
                    expr: r#"monthString("2022-11-14")"#,
                    result: ExecResult::String("Nov".to_string()),
                },
                TestCase {
                    expr: r#"weekdayString(date("2022-11-14"))"#,
                    result: ExecResult::String("Mon".to_string()),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"sum([1, 2, 3])"#,
                    result: ExecResult::Number(dec!(6.0)),
                },
                TestCase {
                    expr: r#"avg([1, 2, 3])"#,
                    result: ExecResult::Number(dec!(2.0)),
                },
                TestCase {
                    expr: r#"min([1, 2, 3])"#,
                    result: ExecResult::Number(dec!(1.0)),
                },
                TestCase {
                    expr: r#"max([1, 2, 3])"#,
                    result: ExecResult::Number(dec!(3.0)),
                },
                TestCase {
                    expr: r#"floor(3.5)"#,
                    result: ExecResult::Number(dec!(3.0)),
                },
                TestCase {
                    expr: r#"ceil(3.5)"#,
                    result: ExecResult::Number(dec!(4.0)),
                },
                TestCase {
                    expr: r#"round(4.7)"#,
                    result: ExecResult::Number(dec!(5.0)),
                },
                TestCase {
                    expr: r#"rand(10) <= 10"#,
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: r#"10 % 4"#,
                    result: ExecResult::Number(dec!(2.0)),
                },
                TestCase {
                    expr: r#"true ? 10.0 == 10 : 1.0"#,
                    result: ExecResult::Bool(true),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"223_000.48 - 120_000_00 / 100"#,
                    result: ExecResult::Number(dec!(103_000.48)),
                },
                TestCase {
                    expr: r#"9223372036854775807"#,
                    result: ExecResult::Number(dec!(9223372036854775807)),
                },
                TestCase {
                    expr: r#"-9223372036854775807"#,
                    result: ExecResult::Number(dec!(-9223372036854775807)),
                },
            ]),
        },
    ]);

    let isolate = Isolate::default();

    for TestEnv { env, cases } in tests {
        isolate.inject_env(&env);

        for TestCase { expr, result } in cases {
            assert_eq!(result, isolate.run_standard(expr).unwrap(), "{}", expr);
        }
    }
}

struct UnaryTestEnv {
    env: Value,
    reference: &'static str,
    cases: Vec<TestCase>,
}

#[test]
fn isolate_unary_tests() {
    let tests = Vec::from([
        UnaryTestEnv {
            env: json!({
                "customer": {
                    "groups": ["admin", "user"],
                    "purchaseAmounts": [100, 200, 400, 800]
                },
            }),
            reference: "customer.groups",
            cases: Vec::from([TestCase {
                expr: r#"some($, # == "admin")"#,
                result: ExecResult::Bool(true),
            }]),
        },
        UnaryTestEnv {
            env: json!({
                "customer": {
                    "groups": ["admin", "user"],
                    "purchaseAmounts": [100, 200, 400, 800]
                }
            }),
            reference: "sum(filter(customer.purchaseAmounts, # < 300))",
            cases: Vec::from([
                TestCase {
                    expr: "300",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: ")100..200(",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "in [100..300]",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "[100, 200, 300]",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "100, 200, 300",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "not in [250, 350]",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "> 250",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "< 350",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "== 300",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: "!= 301",
                    result: ExecResult::Bool(true),
                },
                TestCase {
                    expr: ">= 300 and <= 300",
                    result: ExecResult::Bool(true),
                },
            ]),
        },
    ]);

    let isolate = Isolate::default();
    for UnaryTestEnv {
        env,
        cases,
        reference,
    } in tests
    {
        isolate.inject_env(&env);
        isolate.set_reference(reference).unwrap();

        for TestCase { expr, result } in cases {
            assert_eq!(result, isolate.run(expr).unwrap(), "{}", expr);
        }
    }
}

#[test]
fn variable_serde_test() {
    let env = json!({
        "customer": {
            "groups": ["admin", "user"],
            "purchaseAmounts": [100, 200, 400, 800]
        },
    });

    let bump = Bump::new();
    let _ = Variable::from_serde(&env, &bump);
}

#[test]
fn isolate_test_decimals() {
    let isolate = Isolate::default();
    let result = isolate.run_standard("9223372036854775807").unwrap();
    let value = result.to_value().unwrap();

    assert_eq!(value, Value::from(9223372036854775807i64));
}

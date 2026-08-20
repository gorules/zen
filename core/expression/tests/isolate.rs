use serde_json::{json, Value};

use zen_expression::variable::Variable;
use zen_expression::Isolate;

struct TestEnv {
    env: Value,
    cases: Vec<TestCase>,
}

struct TestCase {
    expr: &'static str,
    result: Value,
}

#[test]
fn isolate_standard_test() {
    let tests = Vec::from([
        TestEnv {
            env: json!({
                "hello": "Hello, ",
                "world": "world!",
            }),
            cases: Vec::from([
                TestCase {
                    expr: "hello + world",
                    result: json!("Hello, world!"),
                },
                TestCase {
                    expr: "hello.nested.null.test",
                    result: json!(null),
                },
            ]),
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
                    result: json!(8),
                },
                TestCase {
                    expr: "b^a",
                    result: json!(216),
                },
                TestCase {
                    expr: "c * b / a",
                    result: json!(2),
                },
                TestCase {
                    expr: "abs(a - b - c)",
                    result: json!(4),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "a": 3.14f64,
                "b": 2,
                "c": 3.141592653589793f64,
                "d": 18446744073709551615u64,
                "e": 9_223_372_036_854_775_807i64,

            }),
            cases: Vec::from([
                TestCase {
                    expr: "a",
                    result: json!(3.14),
                },
                TestCase {
                    expr: "b",
                    result: json!(2),
                },
                TestCase {
                    expr: "e",
                    result: json!(9_223_372_036_854_775_807i64),
                },
                TestCase {
                    expr: "a + b",
                    result: json!(5.14),
                },
                TestCase {
                    expr: "(b + c) - (c + b)",
                    result: json!(0),
                },
                TestCase {
                    expr: "d",
                    result: json!(18446744073709551615u64),
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
                    result: json!(true),
                },
                TestCase {
                    expr: "b - a > c",
                    result: json!(true),
                },
                TestCase {
                    expr: "b < a or a > b",
                    result: json!(false),
                },
                TestCase {
                    expr: "t or f",
                    result: json!(true),
                },
                TestCase {
                    expr: "t and f",
                    result: json!(false),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: "1 in [1..5] and 5 in [1..5]",
                    result: json!(true),
                },
                TestCase {
                    expr: "1 not in (1..5] and 5 not in [1..5)",
                    result: json!(true),
                },
                TestCase {
                    expr: "1 not in [1.01..5] and 5 not in [1..4.99]",
                    result: json!(true),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"date("2022-04-04T21:48:30Z") > date("2022-03-04 21:48:20")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"date("2022-04-04T21:48:30Z") > date("2022-04-04T21:48:40+01:00")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"date("2022-04-04 21:48:10") < date("2022-03-04T21:48:20Z")"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"date("2022-04-04 23:59:59") < date("2022-04-05")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"date("2022-04-05 00:00:01") < date("2022-04-05")"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"date("2022-04-04") > date("2022-03-04")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"date("2022-04-04") in [date("2022-03-04")..date("2022-04-04")]"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"date("2022-04-04") in [date("2022-03-04")..date("2022-04-04"))"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"time("2022-04-04T21:48:30Z") > time("2022-05-04 21:48:20")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"time("21:48:30") > time("2022-05-04T21:48:30+01:00")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"time("2022-04-04 21:48:30") < time("2022-05-04 21:48:20")"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"time("21:48:30") > time("21:48:20")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"time("21:48:19") < time("21:48:20")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"time("21:49") > time("21:48:20")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"duration("60m") == duration("1h")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"duration("24h") >= duration("1d")"#,
                    result: json!(true),
                },
            ]),
        },
        TestEnv {
            env: json!({ "customer": { "firstName": "John", "lastName": "Doe" } }),
            cases: Vec::from([
                TestCase {
                    expr: r#"customer.firstName + " " + customer.lastName"#,
                    result: json!("John Doe"),
                },
                TestCase {
                    expr: r#"startsWith(customer.firstName, "Jo")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"endsWith(customer.firstName + customer.lastName, "oe")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"contains(customer.lastName, "Do")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: "upper(customer.firstName) == 'JOHN'",
                    result: json!(true),
                },
                TestCase {
                    expr: "lower(customer.firstName) == 'john'",
                    result: json!(true),
                },
            ]),
        },
        TestEnv {
            env: json!({ "customer": { "firstName": " John", "middleName": " \n Dan    \n", "lastName": "Doe " } }),
            cases: Vec::from([
                TestCase {
                    expr: r#"trim(customer.firstName) + " " + trim(customer.lastName)"#,
                    result: json!("John Doe"),
                },
                TestCase {
                    expr: "trim(customer.firstName) == 'John'",
                    result: json!(true),
                },
                TestCase {
                    expr: "trim(customer.middleName) == 'Dan'",
                    result: json!(true),
                },
                TestCase {
                    expr: "trim(customer.lastName) == 'Doe'",
                    result: json!(true),
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
                    result: json!(true),
                },
                TestCase {
                    expr: r#"all(customer.purchaseAmounts, # in [100..800])"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"not all(customer.purchaseAmounts, # in (100..800))"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"none(customer.purchaseAmounts, # == 99)"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"all(customer.groups, # == "admin" or # == "user")"#,
                    result: json!(true),
                },
                TestCase {
                    expr: "count(customer.groups, true)",
                    result: json!(2),
                },
                TestCase {
                    expr: "count(customer.purchaseAmounts, # > 150)",
                    result: json!(3),
                },
                TestCase {
                    expr: "map(customer.purchaseAmounts, # + 50)[0] == 150",
                    result: json!(true),
                },
                TestCase {
                    expr: "filter(customer.purchaseAmounts, # >= 200)[0] == 200",
                    result: json!(true),
                },
                TestCase {
                    expr: "sum(customer.purchaseAmounts[0:1]) == 300",
                    result: json!(true),
                },
                TestCase {
                    expr: "one(customer.groups, # == 'admin')",
                    result: json!(true),
                },
                TestCase {
                    expr: "one(['admin', 'admin'], # == 'admin')",
                    result: json!(false),
                },
                TestCase {
                    expr: r#"map(["admin", "user"], "hello " + #)"#,
                    result: json!(["hello admin", "hello user"]),
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
                    result: json!(true),
                },
                TestCase {
                    expr: r#"contains(name, '123')"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"contains(groups, 'admin')"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"contains(groups, 'hello')"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"contains(purchaseAmounts, 100)"#,
                    result: json!(true),
                },
                TestCase {
                    expr: "contains(purchaseAmounts, 150)",
                    result: json!(false),
                },
                TestCase {
                    expr: "len(purchaseAmounts)",
                    result: json!(4),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"dayOfWeek(date("2022-11-08"))"#,
                    result: json!(2),
                },
                TestCase {
                    expr: r#"dayOfMonth(date("2022-11-09"))"#,
                    result: json!(9),
                },
                TestCase {
                    expr: r#"dayOfYear(date("2022-11-10"))"#,
                    result: json!(314),
                },
                TestCase {
                    expr: r#"weekOfYear(date("2022-11-12"))"#,
                    result: json!(45),
                },
                TestCase {
                    expr: r#"monthString(date("2022-11-14"))"#,
                    result: json!("Nov"),
                },
                TestCase {
                    expr: r#"monthString("2022-11-14")"#,
                    result: json!("Nov"),
                },
                TestCase {
                    expr: r#"monthOfYear("2022-11-14")"#,
                    result: json!(11),
                },
                TestCase {
                    expr: r#"weekdayString(date("2022-11-14"))"#,
                    result: json!("Mon"),
                },
                TestCase {
                    expr: r#"year("2022-01-01")"#,
                    result: json!(2022),
                },
                TestCase {
                    expr: r#"year(date("2022-01-01"))"#,
                    result: json!(2022),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"sum([1, 2, 3])"#,
                    result: json!(6),
                },
                TestCase {
                    expr: r#"avg([1, 2, 3])"#,
                    result: json!(2),
                },
                TestCase {
                    expr: r#"min([1, 2, 3])"#,
                    result: json!(1),
                },
                TestCase {
                    expr: r#"max([1, 2, 3])"#,
                    result: json!(3),
                },
                TestCase {
                    expr: r#"floor(3.5)"#,
                    result: json!(3),
                },
                TestCase {
                    expr: r#"ceil(3.5)"#,
                    result: json!(4),
                },
                TestCase {
                    expr: r#"round(4.7)"#,
                    result: json!(5),
                },
                TestCase {
                    expr: r#"rand(10) <= 10"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"10 % 4"#,
                    result: json!(2),
                },
                TestCase {
                    expr: r#"true ? 10.0 == 10 : 1.0"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"median([1, 2, 3])"#,
                    result: json!(2),
                },
                TestCase {
                    expr: r#"median([1, 2, 3, 4])"#,
                    result: json!(2.5),
                },
                TestCase {
                    expr: r#"mode([1, 1, 2, 2, 2, 5, 6, 9])"#,
                    result: json!(2),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"223_000.48 - 120_000_00 / 100"#,
                    result: json!(103_000.48),
                },
                TestCase {
                    expr: r#"9223372036854775807"#,
                    result: json!(9223372036854775807i64),
                },
                TestCase {
                    expr: r#"-9223372036854775807"#,
                    result: json!(-9223372036854775807i64),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "numbers": [[1, 2, 3], [4, 5, 6], [7, 8 ,9]]
            }),
            cases: Vec::from([
                TestCase {
                    expr: r#"numbers[0]"#,
                    result: json!([1, 2, 3]),
                },
                TestCase {
                    expr: r#"map(numbers, sum(#))"#,
                    result: json!([6, 15, 24]),
                },
                TestCase {
                    expr: r#"map(numbers, map(#, # - 1))"#,
                    result: json!([[0, 1, 2], [3, 4, 5], [6, 7, 8]]),
                },
                TestCase {
                    expr: r#"filter(numbers, some(#, # < 5))"#,
                    result: json!([[1, 2, 3], [4, 5, 6]]),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "numbers": [[1, 2, 3], [4, 5, 6], [7, 8 ,9]]
            }),
            cases: Vec::from([
                TestCase {
                    expr: r#"numbers[0]"#,
                    result: json!([1, 2, 3]),
                },
                TestCase {
                    expr: r#"map(numbers, sum(#))"#,
                    result: json!([6, 15, 24]),
                },
                TestCase {
                    expr: r#"map(numbers, map(#, # - 1))"#,
                    result: json!([[0, 1, 2], [3, 4, 5], [6, 7, 8]]),
                },
                TestCase {
                    expr: r#"filter(numbers, some(#, # < 5))"#,
                    result: json!([[1, 2, 3], [4, 5, 6]]),
                },
            ]),
        },
        TestEnv {
            env: json!({
              "cart": [
                { "id": "1", "categories": [{"categoryId": "cat1"}, {"categoryId": "cat2"}] },
                { "id": "2", "categories": [{"categoryId": "cat3"}, {"categoryId": "cat4"}] },
                { "id": "3", "categories": [{"categoryId": "cat1"}, {"categoryId": "cat5"}] }
              ]
            }),
            cases: Vec::from([
                TestCase {
                    expr: r#"filter(cart, some(#.categories, #.categoryId == 'cat1'))"#,
                    result: json!([
                        { "id": "1", "categories": [{"categoryId": "cat1"}, {"categoryId": "cat2"}] },
                        { "id": "3", "categories": [{"categoryId": "cat1"}, {"categoryId": "cat5"}] }
                    ]),
                },
                TestCase {
                    expr: r#"map(cart, map(#.categories, #.categoryId))"#,
                    result: json!([["cat1", "cat2"], ["cat3", "cat4"], ["cat1", "cat5"],]),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "nestedArray": [[1, 2, 3], [4, 5, 6]],
                "mixedArray": [1, [2, 3], 4],
                "mixedTypeArray": [1, {"t": "t"}, "hello", ["a", "b"], [4]]
            }),
            cases: Vec::from([
                TestCase {
                    expr: r#"flatten(nestedArray)"#,
                    result: json!([1, 2, 3, 4, 5, 6]),
                },
                TestCase {
                    expr: r#"flatten(mixedArray)"#,
                    result: json!([1, 2, 3, 4]),
                },
                TestCase {
                    expr: r#"flatten(mixedTypeArray)"#,
                    result: json!([1, {"t": "t"}, "hello", "a", "b", 4]),
                },
                TestCase {
                    expr: r#"flatMap(nestedArray, #)"#,
                    result: json!([1, 2, 3, 4, 5, 6]),
                },
                TestCase {
                    expr: r#"merge(nestedArray)"#,
                    result: json!([1, 2, 3, 4, 5, 6]),
                },
                TestCase {
                    expr: r#"merge([[1, 2], [3, 4], [5]])"#,
                    result: json!([1, 2, 3, 4, 5]),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "objects": [{"a": 1}, {"b": 2}, {"c": 3}],
                "overlapping": [{"a": 1, "b": 2}, {"b": 3, "c": 4}],
            }),
            cases: Vec::from([
                TestCase {
                    expr: r#"merge(objects)"#,
                    result: json!({"a": 1, "b": 2, "c": 3}),
                },
                TestCase {
                    expr: r#"merge(overlapping)"#,
                    result: json!({"a": 1, "b": 3, "c": 4}),
                },
                TestCase {
                    expr: r#"merge([])"#,
                    result: json!({}),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"extract("2022-02-01", "(\d{4})-(\d{2})-(\d{2})")"#,
                    result: json!(["2022-02-01", "2022", "02", "01"]),
                },
                TestCase {
                    expr: r#"all(["babble", "bebble", "bibble", "bobble", "bubble"], matches(#, "b[aeiou]bble"))"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"none(["babble", "bebble", "bibble", "bobble", "bubble"], matches(#, "b[aeiou]bblo"))"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"extract("foo.bar", "(\w+)\.(\w+)")"#,
                    result: json!(["foo.bar", "foo", "bar"]),
                },
            ]),
        },
        TestEnv {
            env: json!({}),
            cases: Vec::from([
                TestCase {
                    expr: r#"some(['a', 'b'], startsWith('a', #))"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"some(['a', 'b'], startsWith(#, 'a'))"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"some(['a', 'b'], startsWith('c', #))"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"some(['a', 'b'], startsWith(#, 'c'))"#,
                    result: json!(false),
                },
            ]),
        },
        TestEnv {
            env: json!({
                "name": "hello",
                "zero": 0,
            }),
            cases: Vec::from([
                TestCase {
                    expr: "name[0]",
                    result: json!("h"),
                },
                TestCase {
                    expr: "name[zero]",
                    result: json!("h"),
                },
                TestCase {
                    expr: "name[4]",
                    result: json!("o"),
                },
                TestCase {
                    expr: "name[100]",
                    result: json!(null),
                },
                TestCase {
                    expr: "name[zero + 100]",
                    result: json!(null),
                },
            ]),
        },
    ]);

    let mut isolate = Isolate::new();

    for TestEnv { env, cases } in tests {
        isolate.set_environment(env.into());

        for TestCase { expr, result } in cases {
            let isolate_result = isolate.run_standard(expr);
            let Ok(response) = isolate_result else {
                assert!(
                    false,
                    "Expression failed: {expr}. Error: {:?}",
                    isolate_result.unwrap_err()
                );
                continue;
            };

            assert_eq!(Variable::from(result), response, "{}", expr);
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
                result: json!(true),
            }]),
        },
        UnaryTestEnv {
            env: json!({
                "input": "2023-01-02"
            }),
            reference: "date(input)",
            cases: Vec::from([
                TestCase {
                    expr: r#"[date("2023-01-01")..date("2023-01-03")]"#,
                    result: json!(true),
                },
                TestCase {
                    expr: r#"[date("2023-02-01")..date("2023-01-03")]"#,
                    result: json!(false),
                },
                TestCase {
                    expr: r#"$ in [date("2023-01-01")..date("2023-01-03")]"#,
                    result: json!(true),
                },
            ]),
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
                    result: json!(true),
                },
                TestCase {
                    expr: ")100..200(",
                    result: json!(true),
                },
                TestCase {
                    expr: "in [100..300]",
                    result: json!(true),
                },
                TestCase {
                    expr: "[100, 200, 300]",
                    result: json!(true),
                },
                TestCase {
                    expr: "100, 200, 300",
                    result: json!(true),
                },
                TestCase {
                    expr: "not in [250, 350]",
                    result: json!(true),
                },
                TestCase {
                    expr: "> 250",
                    result: json!(true),
                },
                TestCase {
                    expr: "< 350",
                    result: json!(true),
                },
                TestCase {
                    expr: "== 300",
                    result: json!(true),
                },
                TestCase {
                    expr: "!= 301",
                    result: json!(true),
                },
                TestCase {
                    expr: ">= 300 and <= 300",
                    result: json!(true),
                },
            ]),
        },
    ]);

    let mut isolate = Isolate::new();
    for UnaryTestEnv {
        env,
        cases,
        reference,
    } in tests
    {
        isolate.set_environment(env.into());
        isolate.set_reference(reference).unwrap();

        for TestCase { expr, result } in cases {
            assert_eq!(result, isolate.run_unary(expr).unwrap(), "{}", expr);
        }
    }
}

#[test]
fn isolate_does_not_mutate_environment() {
    let environment: Variable = json!({ "a": 1, "nested": { "b": 2 } }).into();

    let mut isolate = Isolate::new();
    isolate.set_environment(environment.clone());

    isolate.set_reference("a").unwrap();
    isolate.set_local(Variable::nodes_key(), json!({ "some": "node" }).into());
    isolate.run_standard("x = 1; y = x + a; y").unwrap();
    isolate.insert_dollar("computed", Variable::Number(42.into()));

    assert_eq!(
        environment.to_value(),
        json!({ "a": 1, "nested": { "b": 2 } }),
        "the caller's environment must come back exactly as it went in"
    );

    assert_eq!(
        isolate.run_standard("$nodes.some").unwrap().to_value(),
        json!("node")
    );
    assert_eq!(
        isolate.run_standard("$.computed").unwrap().to_value(),
        json!(42)
    );
    assert_eq!(isolate.run_standard("a").unwrap().to_value(), json!(1));
    let mut root_keys = isolate.run_standard("keys($root)").unwrap().to_value();
    root_keys
        .as_array_mut()
        .unwrap()
        .sort_by_key(|k| k.to_string());
    assert_eq!(
        root_keys,
        json!(["$", "$nodes", "a", "nested"]),
        "`$root` still sees the input and the bindings as one object"
    );
}

#[test]
fn assignments_do_not_leak_between_runs() {
    let environment: Variable = json!({ "a": 1 }).into();

    let mut isolate = Isolate::with_environment(environment.clone());
    assert_eq!(
        isolate.run_standard("b = a + 1; b").unwrap().to_value(),
        json!(2)
    );
    assert_eq!(
        isolate.run_standard("b").unwrap(),
        Variable::Null,
        "an assignment lives for one run only"
    );
    assert_eq!(environment.to_value(), json!({ "a": 1 }));
}

#[cfg(test)]
mod test {
    use anyhow::Context;
    use serde_json::Value;
    use std::env;
    use std::ops::Index;
    use zen_expression::Isolate;

    fn test_csv_standard(csv_data: &str) {
        let mut r = csv::ReaderBuilder::new()
            .delimiter(b';')
            .from_reader(csv_data.as_bytes());

        while let Some(maybe_row) = r.records().next() {
            let Ok(row) = maybe_row else {
                continue;
            };

            let (expression, input_str, output_str) = (row.index(0), row.index(1), row.index(2));
            if expression.starts_with("#") {
                continue;
            }

            let output: Value = serde_json5::from_str(output_str).unwrap();

            let mut isolate = Isolate::new();
            if !input_str.is_empty() {
                let input: Value = serde_json5::from_str(input_str).unwrap();
                isolate.set_environment(input.into());
            }

            let maybe_result = isolate
                .run_standard(expression)
                .context(format!("Expression: {expression}"));
            assert!(maybe_result.is_ok(), "{}", maybe_result.unwrap_err());

            let result = maybe_result.unwrap().to_value();
            assert_eq!(
                result, output,
                "Expression {expression}. Expected: {output}, got: {result}"
            );
        }
    }

    #[test]
    fn isolate_test_decimals() {
        let mut isolate = Isolate::new();
        let result = isolate.run_standard("9223372036854775807").unwrap();

        assert_eq!(result.to_value(), Value::from(9223372036854775807i64));
    }

    #[test]
    fn test_standard_csv() {
        let csv_data = include_str!("data/standard.csv");
        test_csv_standard(csv_data);
    }

    #[test]
    fn test_dates_csv() {
        env::set_var("TZ", "UTC");

        let csv_data = include_str!("data/date.csv");
        test_csv_standard(csv_data);
    }

    #[test]
    fn test_unary_csv() {
        let csv_data = include_str!("data/unary.csv");
        let mut r = csv::ReaderBuilder::new()
            .delimiter(b';')
            .from_reader(csv_data.as_bytes());

        while let Some(maybe_row) = r.records().next() {
            let Ok(row) = maybe_row else {
                continue;
            };

            let (expression, input_str, output_str) = (row.index(0), row.index(1), row.index(2));
            if expression.starts_with("#") {
                continue;
            }

            let output: Value = serde_json5::from_str(output_str).unwrap();

            let mut isolate = Isolate::new();
            if !input_str.is_empty() {
                let input: Value = serde_json5::from_str(input_str).unwrap();
                isolate.set_environment(input.into());
            }

            let result = isolate
                .run_unary(expression)
                .context(format!("Expression: {expression}"))
                .unwrap();

            assert_eq!(
                result, output,
                "Expression {expression}. Expected: {output}, got: {result}"
            );
        }
    }
}

#[test]
fn arithmetic_overflow_errors_instead_of_panicking() {
    let mut isolate = Isolate::new();
    let max = "79228162514264337593543950335";

    assert!(isolate.run_standard(&format!("{max} + 1")).is_err());
    assert!(isolate.run_standard(&format!("-{max} - 1")).is_err());
    assert!(isolate.run_standard(&format!("{max} * 2")).is_err());
    assert!(isolate
        .run_standard(&format!("sum([{max}, {max}])"))
        .is_err());
    assert!(isolate
        .run_standard(&format!("avg([{max}, {max}])"))
        .is_err());
    assert!(isolate
        .run_standard(&format!("median([{max}, {max}])"))
        .is_err());
}

#[test]
fn division_and_modulo_by_zero_return_null() {
    let mut isolate = Isolate::new();

    let division = isolate.run_standard("1 / 0").unwrap();
    assert_eq!(division, Variable::Null);

    let modulo = isolate.run_standard("1 % 0").unwrap();
    assert_eq!(modulo, Variable::Null);
}

#[test]
fn assigned_closure_with_inner_assignment_scopes_nest() {
    let mut isolate = Isolate::new();
    isolate.set_environment(json!({}).into());

    let assigned = isolate
        .run_standard("r = map([1,2,3] as x, (y = x * 2; y + 1)); r")
        .unwrap();
    assert_eq!(assigned, Variable::from(json!([3, 5, 7])));

    let flat_mapped = isolate
        .run_standard("r = flatMap([[1],[2,3]] as x, (y = x; y)); r")
        .unwrap();
    assert_eq!(flat_mapped, Variable::from(json!([1, 2, 3])));

    let nested = isolate
        .run_standard("r = map([1,2] as x, map([10,20] as z, (y = x * z; y))); r")
        .unwrap();
    assert_eq!(nested, Variable::from(json!([[10, 20], [20, 40]])));

    let outer_after_inner = isolate
        .run_standard("a = map([1] as x, (y = x; y)); b = a[0] + 1; b")
        .unwrap();
    assert_eq!(outer_after_inner, Variable::from(json!(2)));
}

#[test]
fn date_add_out_of_range_is_invalid_instead_of_panicking() {
    let mut isolate = Isolate::new();

    let huge_seconds = isolate
        .run_standard("d('2020-01-01').add(10000000000000000, 's').isValid()")
        .unwrap();
    assert_eq!(huge_seconds, Variable::Bool(false));

    let huge_days = isolate
        .run_standard("d('2020-01-01').sub(10000000000000, 'd').isValid()")
        .unwrap();
    assert_eq!(huge_days, Variable::Bool(false));
}

#[test]
fn string_index_at_usize_max_returns_null() {
    let mut isolate = Isolate::new();
    isolate.set_environment(json!({ "s": "abc", "i": 18446744073709551615u64 }).into());

    let result = isolate.run_standard("s[i]").unwrap();
    assert_eq!(result, Variable::Null);
}

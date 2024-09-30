use serde_json::{json, Value};
use zen_expression::variable::Variable;
use zen_tmpl::render;

#[test]
fn test_values_types() {
    struct TestCase {
        template: &'static str,
        context: Value,
        expected: Value,
    }

    let test_cases = vec![
        TestCase {
            template: "{{ null }}",
            context: json!(null),
            expected: json!(null),
        },
        TestCase {
            template: "{{ 1 + 1 }}",
            context: json!(null),
            expected: json!(2),
        },
        TestCase {
            template: "{{ 'hello' }}",
            context: json!(null),
            expected: json!("hello"),
        },
        TestCase {
            template: "{{ true or false }}",
            context: json!(null),
            expected: json!(true),
        },
        TestCase {
            template: "{{ [1, 2, 3] }}",
            context: json!(null),
            expected: json!([1, 2, 3]),
        },
        TestCase {
            template: "{{ customer }}",
            context: json!({ "customer": { "firstName": "John", "lastName": "Doe" } }),
            expected: json!({ "firstName": "John", "lastName": "Doe" }),
        },
    ];

    for test_case in test_cases {
        assert_eq!(
            render(test_case.template, test_case.context.into()).unwrap(),
            Variable::from(test_case.expected)
        );
    }
}

#[test]
fn test_interpolation() {
    struct TestCase {
        template: &'static str,
        context: Value,
        expected: Value,
    }

    let test_cases = vec![
        TestCase {
            template: "{{ null }}s",
            context: json!(null),
            expected: json!("nulls"),
        },
        TestCase {
            template: "{{ 1 + 1 }} hello",
            context: json!(null),
            expected: json!("2 hello"),
        },
        TestCase {
            template: "{{ 'hello' }} world",
            context: json!(null),
            expected: json!("hello world"),
        },
        TestCase {
            template: "{{ true or false }} test",
            context: json!(null),
            expected: json!("true test"),
        },
        TestCase {
            template: "{{ [1, 2, 3] }} array",
            context: json!(null),
            expected: json!("[1,2,3] array"),
        },
    ];

    for test_case in test_cases {
        assert_eq!(
            render(test_case.template, test_case.context.into()).unwrap(),
            Variable::from(test_case.expected)
        );
    }
}

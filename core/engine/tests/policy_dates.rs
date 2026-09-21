use serde_json::json;
use zen_engine::policy::{Cursor, CursorTarget, EvaluateRequest, PolicyWorkspace};

fn workspace(source: &str) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("dates", serde_json::from_value(json!({"blocks":[
        {"id":"customer", "type":"dataModel", "props":{"data":{"name":"customer", "properties":[
            {"id":"since", "name":"since", "type":"date", "array":false, "optional":false},
            {"id":"until", "name":"until", "type":"date", "array":false, "optional":false},
            {"id":"dates", "name":"dates", "type":"date", "array":true, "optional":false},
            {"id":"maybe", "name":"maybe", "type":"date", "array":false, "optional":true},
            {"id":"contact", "name":"contact", "type":"relationship", "target":"contact", "array":false, "optional":false}
        ]}}},
        {"id":"contact", "type":"dataModel", "props":{"data":{"name":"contact", "properties":[
            {"id":"birthday", "name":"birthday", "type":"date", "array":false, "optional":false}
        ]}}},
        {"id":"globals", "type":"dataModel", "props":{"data":{"scope":"global", "name":"", "properties":[
            {"id":"asOf", "name":"asOf", "type":"date", "array":false, "optional":false}
        ]}}},
        {"id":"e", "type":"expression", "props":{"data":{"key":"result", "value":source}}}
    ]})).unwrap());
    ws
}

#[test]
fn declared_dates_preserve_existing_input_and_output_representation() {
    for (source, expected) in [
        ("d(customer.since).year()", json!(2024)),
        ("d(customer.dates[0]).year()", json!(2024)),
        ("d(customer.contact.birthday).year()", json!(2000)),
        ("d(asOf).year()", json!(2025)),
        ("customer.maybe == null", json!(true)),
    ] {
        let ws = workspace(source);
        let input = json!({"asOf":"2025-06-01", "customer":{
            "since":"2024-01-01", "until":"2026-01-01", "dates":["2024-03-01", null],
            "maybe":null, "contact":{"birthday":"2000-02-01"}, "untyped":"2024-01-01"
        }});
        let req = EvaluateRequest {
            policy_path: "dates".into(),
            input: input.clone().into(),
            goals: vec![],
            trace: true,
        };
        let result = ws.evaluate(&req).unwrap();
        let output = serde_json::to_value(&result.output).unwrap();
        assert_eq!(output["result"], expected, "{source}");
        assert_eq!(output["customer"], input["customer"]);
        assert_eq!(output["asOf"], input["asOf"]);
        assert_eq!(serde_json::to_value(&req.input).unwrap(), input);
    }
}

#[test]
fn declared_dates_keep_existing_string_validation() {
    for value in ["UTC", "Japan", "2024-99-99", "not a date"] {
        let result = workspace("asOf")
            .evaluate(&EvaluateRequest {
                policy_path: "dates".into(),
                input: json!({"asOf":value}).into(),
                goals: vec![],
                trace: false,
            })
            .unwrap();
        assert_eq!(
            serde_json::to_value(result.output).unwrap()["result"],
            value
        );
    }
}

#[test]
fn raw_date_strings_report_unsupported_methods_and_ordering() {
    for source in [
        "customer.since.year()",
        "customer.dates[0].year()",
        "customer.contact.birthday.year()",
        "asOf.year()",
        "map(customer.dates as item, item.year())",
        "customer.since < customer.until",
        "asOf > customer.since",
    ] {
        let ws = workspace(source);
        let diagnostics = ws.diagnostics("dates");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.location.block_id.as_deref() == Some("e")),
            "{source}: {diagnostics:?}"
        );
        if source.contains(".year()") {
            assert!(
                diagnostics.iter().any(|d| d.message.contains("Use d(...)")),
                "{source}: {diagnostics:?}"
            );
        }
    }
}

#[test]
fn date_method_completions_require_an_explicit_date_value() {
    for (source, has_year) in [
        ("customer.since.", false),
        ("customer.dates[0].", false),
        ("customer.contact.birthday.", false),
        ("asOf.", false),
        ("d(customer.since).", true),
        ("d(asOf).", true),
    ] {
        let ws = workspace(source);
        let items = ws.completions(&Cursor {
            policy_path: "dates".into(),
            block_id: "e".into(),
            target: CursorTarget::Expression { id: "e".into() },
            pos: source.encode_utf16().count() as u32,
        });
        assert_eq!(
            items.iter().any(|item| item.label == "year"),
            has_year,
            "{source}"
        );
    }
}

#[test]
fn supported_date_string_comparisons_keep_clean_diagnostics_and_results() {
    for (source, expected) in [
        ("d(customer.since).year()", json!(2024)),
        ("d(customer.since) < d(customer.until)", json!(true)),
        ("customer.since < d(customer.until)", json!(true)),
        ("d(customer.until) > customer.since", json!(true)),
        ("customer.since == d(customer.since)", json!(true)),
        (
            "customer.since in [d(customer.since), d(customer.until)]",
            json!(true),
        ),
        (
            "customer.since in [d(customer.since)..d(customer.until)]",
            json!(true),
        ),
    ] {
        let ws = workspace(source);
        let diagnostics = ws.diagnostics("dates");
        assert!(
            diagnostics
                .iter()
                .all(|d| d.location.block_id.as_deref() != Some("e")),
            "{source}: {diagnostics:?}"
        );
        let result = ws
            .evaluate(&EvaluateRequest {
                policy_path: "dates".into(),
                input: json!({"customer":{"since":"2024-01-01", "until":"2026-01-01"}}).into(),
                goals: vec![],
                trace: false,
            })
            .unwrap();
        assert_eq!(
            serde_json::to_value(result.output).unwrap()["result"],
            expected,
            "{source}"
        );
    }
}

#[test]
fn evaluation_errors_do_not_poison_the_workspace() {
    let ws = workspace("d(customer.since).year()");
    for input in [json!(123), json!({}), json!([])] {
        let req = |since| EvaluateRequest {
            policy_path: "dates".into(),
            input: json!({"customer":{"since":since}}).into(),
            goals: vec![],
            trace: false,
        };
        assert!(ws.evaluate(&req(input)).is_err());
        let result = ws.evaluate(&req(json!("2024-01-01"))).unwrap();
        assert_eq!(
            serde_json::to_value(result.output).unwrap()["result"],
            json!(2024)
        );
    }
}

#[test]
fn runtime_expression_errors_do_not_poison_the_workspace() {
    let ws =
        workspace("customer.since == \"bad\" ? customer.since.year() : d(customer.since).year()");
    let req = |since| EvaluateRequest {
        policy_path: "dates".into(),
        input: json!({"customer":{"since":since}}).into(),
        goals: vec![],
        trace: false,
    };
    for _ in 0..3 {
        // This string passes input validation and fails inside the expression VM.
        assert!(ws.evaluate(&req("bad")).is_err());
        let result = ws.evaluate(&req("2024-01-01")).unwrap();
        assert_eq!(
            serde_json::to_value(result.output).unwrap()["result"],
            json!(2024)
        );
    }
}

#[test]
fn global_date_columns_keep_calendar_hints_without_changing_dollar_type() {
    use zen_expression::variable::VariableType;
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(json!({"blocks":[
        {"id":"globals","type":"dataModel","props":{"data":{"scope":"global","name":"","properties":[
            {"id":"asOf","name":"asOf","type":"date","array":false,"optional":false}
        ]}}},
        {"id":"dt","type":"decisionTable","props":{"data":{"hitPolicy":"first",
            "inputs":[{"id":"i","name":"As of","field":"asOf"}],
            "outputs":[{"id":"o","name":"Result","field":"result"}],
            "rules":[{"_id":"r","i":"> d(\"2024-01-01\")","o":"true"}]
        }}}
    ]})).unwrap());
    let mut cursor = Cursor {
        policy_path: "p".into(),
        block_id: "dt".into(),
        pos: 2,
        target: CursorTarget::DecisionTableCell {
            row: "r".into(),
            col: "i".into(),
        },
    };
    assert_eq!(
        ws.slot(&cursor, "> ").unwrap().slot.expected,
        Some(VariableType::Date)
    );
    assert_eq!(
        ws.slot(&cursor, "$.").unwrap().slot.operand,
        Some(VariableType::String)
    );
    cursor.pos = 5;
    assert_eq!(
        ws.slot(&cursor, "d($).").unwrap().slot.operand,
        Some(VariableType::Date)
    );
}

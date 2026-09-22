use std::ops::Index;
use std::rc::Rc;

use zen_expression::intellisense::IntelliSense;
use zen_expression::slot::{LabelResolver, SlotRole, SlotState, ValueOption};
use zen_expression::variable::VariableType;

fn labels() -> LabelResolver {
    Rc::new(|name: &str, value: &str| {
        let label = match (name, value) {
            ("status", "open") => "Open case",
            ("status", "closed") => "Closed",
            ("mood", "happy") => "😀 Happy",
            ("mood", "sad") => "😢 Sad",
            _ => return None,
        };
        Some(label.to_string())
    })
}

#[path = "helpers/slot_scope.rs"]
mod slot_scope;
use slot_scope::{array, base_scope, expected_for, obj, scope_for, status};

fn role_for(spec: &str) -> SlotRole {
    match spec {
        "unary" => SlotRole::Unary,
        "condition" => SlotRole::Condition,
        "value" => SlotRole::Value,
        "path" => SlotRole::Path,
        other => panic!("unknown role {other}"),
    }
}

fn state_name(state: SlotState) -> &'static str {
    match state {
        SlotState::Start => "start",
        SlotState::UnaryStart => "unaryStart",
        SlotState::Value => "value",
        SlotState::ListElement => "listElement",
        SlotState::Range => "range",
        SlotState::Operator => "operator",
        SlotState::Logical => "logical",
        SlotState::Argument => "argument",
        SlotState::Member => "member",
        SlotState::Closure => "closure",
        SlotState::InString => "inString",
        SlotState::Path => "path",
    }
}

fn options_tag(options: &[ValueOption]) -> String {
    options
        .iter()
        .map(|o| {
            if o.label == o.value {
                o.value.clone()
            } else {
                format!("{}={}", o.value, o.label)
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn slots_csv() {
    let csv_data = include_str!("data/slots.csv");
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .quoting(false)
        .flexible(true)
        .from_reader(csv_data.as_bytes());

    let mut is = IntelliSense::new();
    is.set_labels(Some(labels()));
    let mut failures = Vec::new();
    let mut count = 0;

    for record in reader.records() {
        let row = record.expect("csv row");
        if row.len() < 11 || row.index(0).starts_with('#') || row.index(0).is_empty() {
            continue;
        }
        count += 1;
        let role = role_for(row.index(0));
        let unary = row.index(1) == "true";
        let scope = scope_for(row.index(2));
        let expected = expected_for(row.index(3));
        let text = row.index(4);
        let caret = text.find('|').expect("caret");
        let source = format!("{}{}", &text[..caret], &text[caret + 1..]);

        let result = is.slot(
            &source,
            caret as u32,
            unary,
            role,
            &scope,
            expected.as_ref(),
        );
        let slot = result.slot;

        let actual = [
            state_name(slot.state).to_string(),
            slot.expected
                .as_ref()
                .map(|t| t.to_string())
                .unwrap_or_default(),
            options_tag(&slot.options),
            slot.operators.join(","),
            format!("{}..{}", slot.replace_span.0, slot.replace_span.1),
            slot.auto_open.to_string(),
        ];
        let wanted = [
            row.index(5).to_string(),
            row.index(6).to_string(),
            row.index(7).to_string(),
            row.index(8).to_string(),
            row.index(9).to_string(),
            row.index(10).to_string(),
        ];
        if actual != wanted {
            failures.push(format!(
                "{text}\n    wanted {}\n    got    {}",
                wanted.join(" ; "),
                actual.join(" ; ")
            ));
        }
        assert_eq!(result.unary, unary);
    }

    assert!(count >= 80, "only {count} rows");
    assert!(
        failures.is_empty(),
        "{} of {count} rows failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn slot_at(
    text: &str,
    unary: bool,
    role: SlotRole,
    scope: &str,
    expected: &str,
) -> zen_expression::slot::Slot {
    let mut is = IntelliSense::new();
    is.set_labels(Some(labels()));
    let caret = text.find('|').expect("caret");
    let source = format!("{}{}", &text[..caret], &text[caret + 1..]);
    is.slot(
        &source,
        caret as u32,
        unary,
        role,
        &scope_for(scope),
        expected_for(expected).as_ref(),
    )
    .slot
}

#[test]
fn review_cursor_context_regressions() {
    for text in ["map(items as |)", "map(items as x|)"] {
        let slot = slot_at(text, false, SlotRole::Condition, "", "");
        assert!(!slot.auto_open, "{text}");
        assert!(slot.operators.is_empty(), "{text}");
    }
    let glued = slot_at("status ??|", false, SlotRole::Value, "", "status");
    assert_eq!(glued.state, SlotState::Value);
    assert_eq!(glued.replace_span, (9, 9));
    assert!(slot_at("age not i|", false, SlotRole::Condition, "", "")
        .operators
        .contains(&"not in"));
    assert_eq!(
        slot_at(" cust|", false, SlotRole::Path, "", "").state,
        SlotState::Path
    );
    assert!(slot_at("d(customer.|)", false, SlotRole::Condition, "", "")
        .wanted_scalar()
        .is_none());
    for text in ["len(customer.|)", "age in [customer.|]"] {
        assert!(
            !slot_at(text, false, SlotRole::Condition, "", "").can_chain,
            "{text}"
        );
    }
}

#[test]
fn nested_lists_keep_element_expectations() {
    let expected = array(status());
    for text in [
        "[|]",
        "([|])",
        "active ? [|] : []",
        "statuses ?? [|]",
        "{statuses: [|]}",
    ] {
        let wanted = if text.starts_with('{') {
            obj(&[("statuses", expected.clone())])
        } else {
            expected.clone()
        };
        let caret = text.find('|').unwrap();
        let result = IntelliSense::new().slot(
            &text.replace('|', ""),
            caret as u32,
            false,
            SlotRole::Value,
            &base_scope(),
            Some(&wanted),
        );
        assert_eq!(result.slot.expected, Some(status()), "{text}");
        assert_eq!(result.slot.options.len(), 2, "{text}");
    }
}

#[test]
fn enum_options_insert_valid_container_shapes() {
    for text in ["status in |", "status not in |"] {
        let slot = slot_at(text, false, SlotRole::Condition, "", "");
        for option in slot.options {
            let source = text.replace('|', option.source.as_ref().unwrap());
            assert!(
                zen_expression::evaluate_expression(
                    &source,
                    serde_json::json!({"status": "open"}).into()
                )
                .is_ok(),
                "{source}"
            );
        }
    }
    let expected = array(array(status()));
    let result = IntelliSense::new().slot(
        "",
        0,
        false,
        SlotRole::Value,
        &base_scope(),
        Some(&expected),
    );
    assert_eq!(
        result.slot.options[0].source.as_deref(),
        Some("[[\"open\"]]")
    );
}

#[test]
fn string_prefix_and_duplicate_unary_values_follow_the_caret() {
    let slot = slot_at("status == \"op|n\"", false, SlotRole::Condition, "", "");
    assert_eq!(
        slot.options
            .iter()
            .map(|o| o.value.as_str())
            .collect::<Vec<_>>(),
        vec!["open"]
    );
    assert_eq!(slot.replace_span, (10, 15));
    let slot = slot_at("\"open\", \"open|", true, SlotRole::Unary, "$status", "");
    assert!(slot.options.is_empty());
    let scope = base_scope();
    scope.dot_insert("$", VariableType::Nullable(Rc::new(status())));
    let result = IntelliSense::new().slot("null, ", 6, true, SlotRole::Unary, &scope, None);
    assert!(!result
        .slot
        .options
        .iter()
        .any(|o| o.source.as_deref() == Some("null")));
}

#[test]
fn argument_carries_function_and_index() {
    let slot = slot_at("d(|", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.function.as_deref(), Some("d"));
    assert_eq!(slot.argument, Some(0));
    assert_eq!(slot.operand, None);

    let slot = slot_at("contains(name, |", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.function.as_deref(), Some("contains"));
    assert_eq!(slot.argument, Some(1));

    let slot = slot_at("since.isAfter(|", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.function.as_deref(), Some("isAfter"));
    assert_eq!(slot.argument, Some(0));
    assert_eq!(slot.operand, Some(VariableType::Date));
}

#[test]
fn closure_carries_element_type() {
    let slot = slot_at("some(items, |", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.state, SlotState::Closure);
    assert_eq!(slot.function.as_deref(), Some("some"));
    assert_eq!(slot.argument, Some(1));
    assert!(matches!(slot.operand, Some(VariableType::Object(_))));

    let slot = slot_at("some(items, #.|", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.state, SlotState::Member);
    assert!(matches!(slot.operand, Some(VariableType::Object(_))));
}

#[test]
fn listed_values_are_removed() {
    let slot = slot_at("\"open\", |", true, SlotRole::Unary, "$status", "");
    assert_eq!(slot.listed, vec!["open".to_string()]);
    assert_eq!(slot.options.len(), 1);
    assert_eq!(slot.options[0].value, "closed");
    assert_eq!(slot.options[0].source.as_deref(), Some("\"closed\""));

    let slot = slot_at(
        "status in [\"closed\", |",
        false,
        SlotRole::Condition,
        "",
        "bool",
    );
    assert_eq!(slot.listed, vec!["closed".to_string()]);
    assert_eq!(slot.options[0].value, "open");
    assert_eq!(slot.options[0].label, "Open case");

    let slot = slot_at("[\"open\"], \"|", true, SlotRole::Unary, "$status", "");
    assert_eq!(slot.state, SlotState::InString);
    assert_eq!(slot.listed, vec!["open".to_string()]);
}

#[test]
fn in_string_reports_quote_and_operand() {
    let slot = slot_at("status == '|", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.in_string, Some('\''));
    assert_eq!(slot.operand, Some(status()));
    assert!(zen_expression::evaluate_expression(
        "status == '",
        serde_json::json!({ "status": "open" }).into(),
    )
    .is_err());

    let slot = slot_at(
        "status == \"open\"|",
        false,
        SlotRole::Condition,
        "",
        "bool",
    );
    assert_eq!(slot.in_string, None);
    assert_eq!(slot.state, SlotState::Logical);

    let slot = slot_at("`a ${|}`", false, SlotRole::Value, "", "string");
    assert_eq!(slot.state, SlotState::Start);
    assert_eq!(slot.in_string, None);
}

#[test]
fn operand_types_survive_unclosed_parens() {
    let slot = slot_at("(customer.age |", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.state, SlotState::Operator);
    assert_eq!(slot.operand, Some(VariableType::Number));

    let slot = slot_at(
        "(status == \"open\" or status == |",
        false,
        SlotRole::Condition,
        "",
        "bool",
    );
    assert_eq!(slot.operand, Some(status()));
}

#[test]
fn unicode_byte_spans() {
    let text = "name == \"Ünïcode\" and status == \"|";
    let slot = slot_at(text, false, SlotRole::Condition, "", "bool");
    let caret = text.find('|').unwrap() as u32;
    assert_eq!(slot.replace_span, (caret - 1, caret));
    assert_eq!(slot.options.len(), 2);

    let slot = slot_at("mood == \"😀|", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.replace_span, (8, 13));
    assert_eq!(slot.options[0].label, "😀 Happy");

    let source = "mood == \"😀 Happy\"";
    let mut is = IntelliSense::new();
    let inside = is.slot(
        source,
        11,
        false,
        SlotRole::Condition,
        &base_scope(),
        Some(&VariableType::Bool),
    );
    assert_eq!(inside.slot.state, SlotState::InString);
    assert_eq!(inside.slot.replace_span, (8, source.len() as u32));
}

#[test]
fn slot_includes_literal_facts() {
    let mut is = IntelliSense::new();
    is.set_labels(Some(labels()));
    let source = "status == \"open\" and since > d(\"2024-01-01\") and tier == ";
    let result = is.slot(
        source,
        source.len() as u32,
        false,
        SlotRole::Condition,
        &base_scope(),
        Some(&VariableType::Bool),
    );
    assert_eq!(result.slot.state, SlotState::Value);
    assert_eq!(result.literals.len(), 2);
    assert_eq!(result.enums.len(), 1);
    assert_eq!(result.enums[0].name.as_deref(), Some("status"));
    assert_eq!(result.enums[0].options[0].label, "Open case");
}

#[test]
fn assignments_and_semicolons() {
    let slot = slot_at("age > 1;|", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.state, SlotState::Start);
    assert_eq!(slot.expected, Some(VariableType::Bool));

    let slot = slot_at("x = 1; |", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.state, SlotState::Start);
    assert_eq!(slot.expected, Some(VariableType::Bool));

    let slot = slot_at(
        "x = 1; status == \"|",
        false,
        SlotRole::Condition,
        "",
        "bool",
    );
    assert_eq!(slot.state, SlotState::InString);
    assert_eq!(slot.options.len(), 2);
    assert_eq!(slot.replace_span, (17, 18));

    let slot = slot_at("x = 1; x |", false, SlotRole::Condition, "", "bool");
    assert_eq!(slot.state, SlotState::Operator);
}

#[test]
fn closure_locals_follow_the_caret() {
    let names = |slot: &zen_expression::slot::Slot| -> Vec<String> {
        slot.locals.iter().map(|l| l.name.clone()).collect()
    };
    let element = |slot: &zen_expression::slot::Slot, i: usize| slot.locals[i].kind.to_string();

    let slot = slot_at("map(items as x, |", false, SlotRole::Value, "", "");
    assert_eq!(slot.state, SlotState::Closure);
    assert_eq!(names(&slot), ["x"]);
    assert!(matches!(slot.locals[0].kind, VariableType::Object(_)));
    assert!(matches!(slot.operand, Some(VariableType::Object(_))));

    let slot = slot_at("map(items as x, x|", false, SlotRole::Value, "", "");
    assert_eq!(names(&slot), ["x"]);
    assert_eq!(slot.replace_span, (16, 17));

    let nested =
        r#"{"Object":{"m":{"Array":{"Object":{"a":"Number","tags":{"Array":"String"}}}}}}"#;
    let slot = slot_at(
        "map(m as x, map(x.tags as y, |",
        false,
        SlotRole::Value,
        nested,
        "",
    );
    assert_eq!(names(&slot), ["y", "x"]);
    assert_eq!(element(&slot, 0), "string");
    assert!(matches!(slot.locals[1].kind, VariableType::Object(_)));

    let slot = slot_at("map(m, map(#.tags, |", false, SlotRole::Value, nested, "");
    assert_eq!(names(&slot), ["#"]);
    assert_eq!(element(&slot, 0), "string");

    let slot = slot_at("map(items, |", false, SlotRole::Value, "", "");
    assert_eq!(names(&slot), ["#"]);
    assert!(matches!(slot.locals[0].kind, VariableType::Object(_)));

    let slot = slot_at(
        "filter(items as x, x.price > |",
        false,
        SlotRole::Condition,
        "",
        "bool",
    );
    assert_eq!(slot.state, SlotState::Value);
    assert_eq!(names(&slot), ["x"]);

    let slot = slot_at("map(items as x, len(x|", false, SlotRole::Value, "", "");
    assert_eq!(names(&slot), ["x"]);

    assert!(slot_at(
        "map(items as x, x.price) + |",
        false,
        SlotRole::Value,
        "",
        ""
    )
    .locals
    .is_empty());
    assert!(slot_at("map(|", false, SlotRole::Value, "", "")
        .locals
        .is_empty());
}

use std::rc::Rc;

use zen_expression::intellisense::IntelliSense;
use zen_expression::nl::{
    encode_string, EditHint, EnumOption, NlRequest, NlResult, NlTokenKind, OpChoice, OpSym,
    TypeTag, WordSym,
};
use zen_expression::variable::VariableType;

fn op(sym: OpSym) -> NlTokenKind {
    NlTokenKind::Op {
        sym,
        implied: false,
        between: false,
    }
}

fn op_implied(sym: OpSym) -> NlTokenKind {
    NlTokenKind::Op {
        sym,
        implied: true,
        between: false,
    }
}

fn choices(syms: &[OpSym]) -> Vec<OpChoice> {
    syms.iter().copied().map(OpChoice::from).collect()
}

fn obj(fields: &[(&str, VariableType)]) -> VariableType {
    let object = VariableType::empty_object();
    if let VariableType::Object(map) = &object {
        for (key, value) in fields {
            map.borrow_mut().insert(Rc::from(*key), value.clone());
        }
    }
    object
}

fn enum_t(name: &str, values: &[&str]) -> VariableType {
    VariableType::Enum(
        Some(Rc::from(name)),
        values.iter().map(|v| Rc::from(*v)).collect(),
    )
}

fn array(inner: VariableType) -> VariableType {
    VariableType::Array(Rc::new(inner))
}

fn run(expr: &str, unary: bool, subject: Option<VariableType>, root: &VariableType) -> NlResult {
    let mut intellisense = IntelliSense::new();
    intellisense.nl_tokenize(
        &NlRequest {
            id: "x".into(),
            expression: expr.into(),
            unary,
            subject_type: subject,
        },
        root,
    )
}

fn kinds(result: &NlResult) -> Vec<NlTokenKind> {
    result.tokens.iter().map(|t| t.token.clone()).collect()
}

#[test]
fn comparison_number() {
    let root = obj(&[("customer", obj(&[("age", VariableType::Number)]))]);
    let result = run("customer.age >= 18", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(
        kinds(&result),
        vec![
            NlTokenKind::Field {
                path: vec!["customer".into(), "age".into()],
                ty: TypeTag::Number,
            },
            op(OpSym::Gte),
            NlTokenKind::Number { value: "18".into() },
        ]
    );

    let number = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Number { .. }))
        .unwrap();
    assert_eq!(number.hint, None);
}

#[test]
fn enum_membership_multiselect() {
    let root = obj(&[("tier", enum_t("Tier", &["gold", "silver", "bronze"]))]);
    let result = run("tier in ['gold', 'silver']", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Field {
            path: vec!["tier".into()],
            ty: TypeTag::Enum { index: 0 },
        }
    );
    assert_eq!(k[1], op(OpSym::In));
    assert_eq!(
        k[2],
        NlTokenKind::EnumList {
            selected: vec!["gold".into(), "silver".into()],
        }
    );
    assert_eq!(k.len(), 3);
    assert_eq!(
        result.tokens[2].hint,
        Some(EditHint::MultiSelect { options: 0 })
    );

    assert_eq!(
        result.enums,
        vec![vec![
            EnumOption {
                label: "gold".to_string(),
                source: Some("\"gold\"".to_string()),
            },
            EnumOption {
                label: "silver".to_string(),
                source: Some("\"silver\"".to_string()),
            },
            EnumOption {
                label: "bronze".to_string(),
                source: Some("\"bronze\"".to_string()),
            },
        ]]
    );
}

#[test]
fn enum_list_spans_whole_array() {
    let root = obj(&[("tier", enum_t("Tier", &["gold", "silver"]))]);
    let result = run("tier in ['gold']", false, None, &root);

    let list = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::EnumList { .. }))
        .unwrap();
    let source = "tier in ['gold']";
    assert_eq!(
        &source[list.span.0 as usize..list.span.1 as usize],
        "['gold']"
    );
}

#[test]
fn mixed_enum_array_stays_plain_list() {
    let root = obj(&[
        ("tier", enum_t("Tier", &["gold", "silver"])),
        ("other", VariableType::String),
    ]);
    let result = run("tier in ['gold', other]", false, None, &root);

    let k = kinds(&result);
    assert!(k.contains(&NlTokenKind::ListOpen));
    assert!(k.contains(&NlTokenKind::ListClose));
    assert!(!k.iter().any(|t| matches!(t, NlTokenKind::EnumList { .. })));
}

#[test]
fn encode_string_swaps_quotes() {
    assert_eq!(encode_string("plain"), Some("\"plain\"".to_string()));
    assert_eq!(encode_string("6\" nail"), Some("'6\" nail'".to_string()));
    assert_eq!(encode_string("it's"), Some("\"it's\"".to_string()));
    assert_eq!(encode_string("both \" and '"), None);
}

#[test]
fn closure_elides_top_level_alias() {
    let root = obj(&[("countries", array(enum_t("Country", &["US", "CA", "GB"])))]);
    let result = run("all(countries as c, c == 'US')", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Func {
            sym: "all".into(),
            closure: true,
        }
    );
    assert!(matches!(&k[1], NlTokenKind::Field { .. }));
    assert_eq!(k[2], op(OpSym::Eq));
    assert!(matches!(&k[3], NlTokenKind::Str { .. }));
}

#[test]
fn closure_elides_alias_in_member_paths() {
    let root = obj(&[(
        "order",
        obj(&[("items", array(obj(&[("price", VariableType::Number)])))]),
    )]);
    let result = run(
        "all(order.items as item, item.price > 100)",
        false,
        None,
        &root,
    );

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Func {
            sym: "all".into(),
            closure: true,
        }
    );
    assert!(
        matches!(&k[1], NlTokenKind::Field { path, .. } if path.len() == 2 && path[0].as_ref() == "order")
    );
    assert_eq!(k[2], NlTokenKind::Word { sym: WordSym::Has });
    assert!(
        matches!(&k[3], NlTokenKind::Field { path, .. } if path.len() == 1 && path[0].as_ref() == "price")
    );
    assert_eq!(k[4], op_implied(OpSym::Gt));
}

#[test]
fn closure_callback_reference() {
    let root = obj(&[("orders", array(VariableType::Number))]);
    let result = run("some(orders, # > 100)", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Func {
            sym: "some".into(),
            closure: true,
        }
    );
    assert!(matches!(&k[1], NlTokenKind::Field { .. }));
    assert_eq!(k[2], op(OpSym::Gt));
}

#[test]
fn unary_number_cell() {
    let result = run("> 18", true, Some(VariableType::Number), &VariableType::Any);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(k[0], op_implied(OpSym::Gt));
    assert_eq!(k[1], NlTokenKind::Number { value: "18".into() });
}

#[test]
fn unary_enum_cell() {
    let subject = enum_t("Tier", &["gold", "silver"]);
    let result = run("'gold'", true, Some(subject), &VariableType::Any);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(k[0], op_implied(OpSym::Eq));
    assert!(matches!(&k[1], NlTokenKind::Str { .. }));
    assert_eq!(result.tokens[1].hint, Some(EditHint::Select { options: 0 }));
}

#[test]
fn function_call_projects_args() {
    let root = obj(&[("scores", array(VariableType::Number))]);
    let result = run("sum(scores)", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Func {
            sym: "sum".into(),
            closure: false,
        }
    );
    assert!(matches!(&k[1], NlTokenKind::Field { .. }));
    assert_eq!(k.len(), 2);
}

#[test]
fn assignments_project_statements() {
    let root = obj(&[("base", VariableType::Number)]);
    let result = run("x = base * 2; y = x + 1; y > 10", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let k = kinds(&result);
    let tags: Vec<&str> = k
        .iter()
        .map(|t| match t {
            NlTokenKind::Field { .. } => "field",
            NlTokenKind::Assign => "assign",
            NlTokenKind::StmtEnd => "stmtEnd",
            NlTokenKind::Op { .. } => "op",
            NlTokenKind::Number { .. } => "number",
            other => panic!("unexpected token {other:?}"),
        })
        .collect();
    assert_eq!(
        tags,
        vec![
            "field", "assign", "field", "op", "number", "stmtEnd", "field", "assign", "field",
            "op", "number", "stmtEnd", "field", "op", "number",
        ]
    );
    assert_eq!(
        k[0],
        NlTokenKind::Field {
            path: vec!["x".into()],
            ty: TypeTag::Number,
        }
    );
}

#[test]
fn constant_index_member_projects_as_field() {
    let root = obj(&[(
        "revenue",
        obj(&[(
            "tiers",
            array(obj(&[("itdReceipts", VariableType::Number)])),
        )]),
    )]);
    let result = run("revenue.tiers[0].itdReceipts > 0", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(
        kinds(&result),
        vec![
            NlTokenKind::Field {
                path: vec!["revenue".into(), "tiers[0]".into(), "itdReceipts".into()],
                ty: TypeTag::Number,
            },
            op(OpSym::Gt),
            NlTokenKind::Number { value: "0".into() },
        ]
    );
}

#[test]
fn dynamic_index_member_stays_code() {
    let root = obj(&[
        ("items", array(VariableType::Number)),
        ("i", VariableType::Number),
    ]);
    let result = run("items[i]", false, None, &root);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Code {
            source: "items[i]".into(),
        }
    );
}

#[test]
fn slice_falls_back_to_code() {
    let root = obj(&[("items", array(VariableType::Number))]);
    let result = run("items[1:3]", false, None, &root);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Code {
            source: "items[1:3]".into(),
        }
    );
}

#[test]
fn unknown_function_half_stays_structured() {
    let root = obj(&[("name", VariableType::String)]);
    let result = run("fuzzyMatch(name, 'jon') > 0.8", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert!(matches!(&k[0], NlTokenKind::Field { .. }));
    assert_eq!(
        k[1],
        NlTokenKind::Func {
            sym: "fuzzyMatch".into(),
            closure: false,
        }
    );
    assert!(matches!(&k[2], NlTokenKind::Str { .. }));
    assert!(k.iter().any(|t| *t == op(OpSym::Gt)));
}

#[test]
fn date_comparison_hints_date_picker() {
    let root = obj(&[("start", VariableType::Date)]);
    let result = run("start >= '2026-01-01'", false, None, &root);

    let date = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .unwrap();
    assert_eq!(date.hint, Some(EditHint::DatePicker));
}

#[test]
fn conditional_projects_words() {
    let root = obj(&[("age", VariableType::Number)]);
    let result = run("age > 18 ? 'adult' : 'minor'", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(k[0], NlTokenKind::Word { sym: WordSym::If });
    assert!(matches!(&k[1], NlTokenKind::Field { .. }));
    assert_eq!(k[2], op(OpSym::Gt));
    assert!(matches!(&k[3], NlTokenKind::Number { .. }));
    assert_eq!(k[4], NlTokenKind::Word { sym: WordSym::Then });
    assert!(matches!(&k[5], NlTokenKind::Str { .. }));
    assert_eq!(
        k[6],
        NlTokenKind::Word {
            sym: WordSym::Otherwise,
        }
    );
    assert!(matches!(&k[7], NlTokenKind::Str { .. }));
}

#[test]
fn template_string_projects_parts() {
    let root = obj(&[("name", VariableType::String)]);
    let result = run("`Hi ${name}!`", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(k[0], NlTokenKind::TemplateOpen);
    assert_eq!(
        k[1],
        NlTokenKind::TemplateText {
            value: "Hi ".into(),
        }
    );
    assert_eq!(
        k[2],
        NlTokenKind::Field {
            path: vec!["name".into()],
            ty: TypeTag::String,
        }
    );
    assert_eq!(k[3], NlTokenKind::TemplateText { value: "!".into() });
    assert_eq!(k[4], NlTokenKind::TemplateClose);
}

#[test]
fn unary_interval_projects_range() {
    let result = run(
        "[18..30)",
        true,
        Some(VariableType::Number),
        &VariableType::Any,
    );

    let k = kinds(&result);
    assert!(k.contains(&NlTokenKind::IntervalOpen { inclusive: true }));
    assert!(k.contains(&NlTokenKind::Word {
        sym: WordSym::RangeAnd,
    }));
    assert!(k.contains(&NlTokenKind::IntervalClose { inclusive: false }));
}

#[test]
fn membership_over_interval_marks_between() {
    let root = obj(&[("age", VariableType::Number)]);
    let result = run("age in [18..65]", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let k = kinds(&result);
    assert!(k.contains(&NlTokenKind::Op {
        sym: OpSym::In,
        implied: false,
        between: true,
    }));
}

#[test]
fn op_choices_carry_source() {
    let root = obj(&[("tier", enum_t("Tier", &["gold", "silver"]))]);
    let result = run("tier != 'gold'", false, None, &root);

    let op_tok = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Op { .. }))
        .unwrap();
    let Some(EditHint::OpSelect { options }) = &op_tok.hint else {
        panic!("expected opSelect hint");
    };
    let sources: Vec<&str> = options.iter().map(|o| o.source).collect();
    assert_eq!(sources, vec!["==", "!="]);
}

#[test]
fn method_call_projects_receiver_and_args() {
    let root = obj(&[("created", VariableType::Date)]);
    let result = run("created.format('yyyy-MM-dd')", false, None, &root);

    let k = kinds(&result);
    assert!(matches!(&k[0], NlTokenKind::Field { .. }));
    assert_eq!(
        k[1],
        NlTokenKind::Method {
            sym: "format".into(),
        }
    );
    assert!(matches!(&k[2], NlTokenKind::Str { .. }));
    assert_eq!(k.len(), 3);
}

#[test]
fn nested_closures_shadow_alias() {
    let root = obj(&[(
        "teams",
        array(obj(&[("members", array(VariableType::String))])),
    )]);
    let result = run(
        "some(teams as t, some(t.members as m, m == 'lee'))",
        false,
        None,
        &root,
    );

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    let elements: Vec<_> = k
        .iter()
        .filter_map(|t| match t {
            NlTokenKind::Element { alias } => Some(alias.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(elements, vec![Some("m".into()), Some("m".into())]);
    assert!(!k
        .iter()
        .any(|t| matches!(t, NlTokenKind::Field { path, .. } if path == &vec![Box::from("m")])));
}

#[test]
fn json_wire_shape() {
    let root = obj(&[("customer", obj(&[("age", VariableType::Number)]))]);
    let result = run("customer.age >= 18", false, None, &root);

    let value = serde_json::to_value(&result).unwrap();
    let tokens = value["tokens"].as_array().unwrap();

    assert_eq!(tokens[0]["token"]["t"], "field");
    assert_eq!(tokens[0]["token"]["path"][0], "customer");
    assert_eq!(tokens[0]["token"]["ty"]["t"], "number");
    assert_eq!(tokens[1]["token"]["t"], "op");
    assert_eq!(tokens[1]["token"]["sym"], "gte");
    assert_eq!(tokens[2]["token"]["t"], "number");
    assert_eq!(tokens[2]["token"]["value"], "18");
    assert_eq!(tokens[2].get("hint"), None);
}

#[test]
fn batch_correlates_ids() {
    let root = obj(&[("age", VariableType::Number)]);
    let mut intellisense = IntelliSense::new();
    let requests = vec![
        NlRequest {
            id: "a".into(),
            expression: "age > 1".into(),
            unary: false,
            subject_type: None,
        },
        NlRequest {
            id: "b".into(),
            expression: "age < 5".into(),
            unary: false,
            subject_type: None,
        },
    ];

    let results = intellisense.nl_tokenize_batch(&requests, &root);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].id, "a");
    assert_eq!(results[1].id, "b");
}

#[test]
fn op_hint_ordered_for_numbers() {
    let root = obj(&[("customer", obj(&[("age", VariableType::Number)]))]);
    let result = run("customer.age >= 18", false, None, &root);

    let op = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Op { .. }))
        .unwrap();
    assert_eq!(
        op.hint,
        Some(EditHint::OpSelect {
            options: choices(&[
                OpSym::Gt,
                OpSym::Gte,
                OpSym::Lt,
                OpSym::Lte,
                OpSym::Eq,
                OpSym::Ne,
            ])
        })
    );
}

#[test]
fn op_hint_equality_only_for_enums() {
    let root = obj(&[(
        "customer",
        obj(&[("tier", enum_t("Tier", &["gold", "silver"]))]),
    )]);
    let result = run("customer.tier == 'gold'", false, None, &root);

    let op = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Op { .. }))
        .unwrap();
    assert_eq!(
        op.hint,
        Some(EditHint::OpSelect {
            options: choices(&[OpSym::Eq, OpSym::Ne])
        })
    );
}

#[test]
fn op_hint_membership_and_joiners() {
    let root = obj(&[
        ("age", VariableType::Number),
        ("tier", enum_t("Tier", &["gold", "silver"])),
    ]);
    let result = run("age > 18 and tier in ['gold']", false, None, &root);

    let ops: Vec<_> = result
        .tokens
        .iter()
        .filter(|t| matches!(t.token, NlTokenKind::Op { .. }))
        .collect();
    assert_eq!(ops.len(), 3);
    assert_eq!(
        ops[1].hint,
        Some(EditHint::OpSelect {
            options: choices(&[OpSym::And, OpSym::Or])
        })
    );
    assert_eq!(
        ops[2].hint,
        Some(EditHint::OpSelect {
            options: choices(&[OpSym::In, OpSym::NotIn])
        })
    );
}

#[test]
fn infix_predicate_functions() {
    let root = obj(&[("name", VariableType::String)]);
    let result = run("contains(name, 'jo')", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert!(matches!(&k[0], NlTokenKind::Field { .. }));
    assert_eq!(
        k[1],
        NlTokenKind::Func {
            sym: "contains".into(),
            closure: false,
        }
    );
    assert!(matches!(&k[2], NlTokenKind::Str { .. }));
    assert_eq!(k.len(), 3);
}

#[test]
fn complex_single_arg_keeps_group() {
    let root = obj(&[("total", VariableType::Number)]);
    let result = run("round(total * 0.1)", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(
        k[0],
        NlTokenKind::Func {
            sym: "round".into(),
            closure: false,
        }
    );
    assert_eq!(k[1], NlTokenKind::GroupOpen);
    assert_eq!(*k.last().unwrap(), NlTokenKind::GroupClose);
}

fn ticket_array() -> VariableType {
    array(enum_t("Ticket", &["award", "regular", "upgrade"]))
}

fn quant_hint(result: &NlResult) -> (&EditHint, NlTokenKind) {
    let token = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Op { .. }))
        .unwrap();
    (token.hint.as_ref().unwrap(), token.token.clone())
}

#[test]
fn contains_any_unary() {
    let result = run(
        "some($, # in ['award'])",
        true,
        Some(ticket_array()),
        &VariableType::Any,
    );

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(k[0], op_implied(OpSym::ContainsAny));
    assert_eq!(
        k[1],
        NlTokenKind::EnumList {
            selected: vec!["award".into()],
        }
    );
    assert_eq!(k.len(), 2);

    let (hint, _) = quant_hint(&result);
    let EditHint::QuantSelect {
        options,
        subject,
        list,
    } = hint
    else {
        panic!("expected quant select, got {hint:?}");
    };
    assert_eq!(subject.as_ref(), "$");
    assert_eq!(list.as_ref(), "['award']");
    assert_eq!(
        options,
        &vec![
            OpSym::ContainsAny,
            OpSym::ContainsAll,
            OpSym::ContainsNone,
            OpSym::ContainsOnly,
        ]
    );
}

#[test]
fn contains_all_flipped_unary() {
    let result = run(
        "all(['award', 'regular'], # in $)",
        true,
        Some(ticket_array()),
        &VariableType::Any,
    );

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(k[0], op_implied(OpSym::ContainsAll));
    assert_eq!(
        k[1],
        NlTokenKind::EnumList {
            selected: vec!["award".into(), "regular".into()],
        }
    );

    let (hint, _) = quant_hint(&result);
    let EditHint::QuantSelect { subject, list, .. } = hint else {
        panic!("expected quant select, got {hint:?}");
    };
    assert_eq!(subject.as_ref(), "$");
    assert_eq!(list.as_ref(), "['award', 'regular']");
}

#[test]
fn contains_none_unary() {
    let result = run(
        "none($, # in ['award'])",
        true,
        Some(ticket_array()),
        &VariableType::Any,
    );
    assert_eq!(kinds(&result)[0], op_implied(OpSym::ContainsNone));

    let negated = run(
        "all($, # not in ['award'])",
        true,
        Some(ticket_array()),
        &VariableType::Any,
    );
    assert_eq!(kinds(&negated)[0], op_implied(OpSym::ContainsNone));
}

#[test]
fn contains_only_unary() {
    let result = run(
        "all($, # in ['award', 'regular'])",
        true,
        Some(ticket_array()),
        &VariableType::Any,
    );
    assert_eq!(kinds(&result)[0], op_implied(OpSym::ContainsOnly));
}

#[test]
fn contains_any_field_subject() {
    let root = obj(&[(
        "order",
        obj(&[("tags", array(enum_t("Tag", &["vip", "fragile"])))]),
    )]);
    let result = run("some(order.tags, # in ['vip'])", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert!(
        matches!(&k[0], NlTokenKind::Field { path, .. } if path.len() == 2 && path[0].as_ref() == "order")
    );
    assert_eq!(k[1], op(OpSym::ContainsAny));
    assert_eq!(
        k[2],
        NlTokenKind::EnumList {
            selected: vec!["vip".into()],
        }
    );
}

#[test]
fn contains_single_flip_unary() {
    let result = run(
        "'award' in $",
        true,
        Some(ticket_array()),
        &VariableType::Any,
    );

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);

    let k = kinds(&result);
    assert_eq!(k[0], op_implied(OpSym::Contains));
    assert_eq!(
        k[1],
        NlTokenKind::Str {
            value: "award".into(),
        }
    );

    let op_token = &result.tokens[0];
    assert_eq!(
        op_token.hint,
        Some(EditHint::OpSelect {
            options: choices(&[OpSym::Contains, OpSym::NotContains]),
        })
    );
    let needle = &result.tokens[1];
    assert!(matches!(needle.hint, Some(EditHint::Select { .. })));

    let negated = run(
        "'award' not in $",
        true,
        Some(ticket_array()),
        &VariableType::Any,
    );
    assert_eq!(kinds(&negated)[0], op_implied(OpSym::NotContains));
}

#[test]
fn contains_flip_keeps_literal_lists() {
    let root = obj(&[("tier", enum_t("Tier", &["gold", "silver"]))]);
    let result = run("tier in ['gold']", false, None, &root);

    let k = kinds(&result);
    assert!(!k.iter().any(|kind| matches!(
        kind,
        NlTokenKind::Op {
            sym: OpSym::Contains,
            ..
        }
    )));
}

#[test]
fn quantifier_func_select() {
    let root = obj(&[("orders", array(VariableType::Number))]);
    let result = run("some(orders, # > 100)", false, None, &root);

    let func = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Func { .. }))
        .unwrap();
    assert_eq!(func.span, (0, 4));
    let Some(EditHint::FuncSelect { options }) = &func.hint else {
        panic!("expected func select, got {:?}", func.hint);
    };
    assert_eq!(
        options
            .iter()
            .map(|option| option.as_ref())
            .collect::<Vec<_>>(),
        vec!["all", "some", "none"]
    );
}

#[test]
fn non_array_subject_falls_through() {
    let root = obj(&[("name", VariableType::String)]);
    let result = run("some(name, # in ['a'])", false, None, &root);

    let k = kinds(&result);
    assert!(matches!(
        &k[0],
        NlTokenKind::Func { sym, closure: true } if sym.as_ref() == "some"
    ));
}

#[test]
fn date_constructor_string_elides_func_and_hints_picker() {
    let root = obj(&[("startDate", VariableType::Date)]);
    let result = run("startDate > d('2024-01-15')", false, None, &root);

    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    let k = kinds(&result);
    assert!(!k
        .iter()
        .any(|t| matches!(t, NlTokenKind::Func { .. } | NlTokenKind::GroupOpen)));
    let date = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .unwrap();
    assert_eq!(
        date.token,
        NlTokenKind::Str {
            value: "2024-01-15".into()
        }
    );
    assert_eq!(date.hint, Some(EditHint::DatePicker));
    assert_eq!(date.span, (14, 26));
}

#[test]
fn date_constructor_with_timezone_keeps_func_and_hints_first_arg() {
    let root = obj(&[("startDate", VariableType::Date)]);
    let result = run(
        "startDate > d('2024-01-15 10:30', 'Europe/Berlin')",
        false,
        None,
        &root,
    );

    let k = kinds(&result);
    assert!(k
        .iter()
        .any(|t| matches!(t, NlTokenKind::Func { sym, closure: false } if sym.as_ref() == "d")));
    let strs: Vec<_> = result
        .tokens
        .iter()
        .filter(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .collect();
    assert_eq!(strs.len(), 2);
    assert_eq!(strs[0].hint, Some(EditHint::DatePicker));
    assert_eq!(strs[1].hint, None);
}

#[test]
fn bare_string_compared_to_date_field_hints_picker() {
    let root = obj(&[("startDate", VariableType::Date)]);
    let result = run("startDate >= '2024-01-15'", false, None, &root);

    let date = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .unwrap();
    assert_eq!(date.hint, Some(EditHint::DatePicker));
}

#[test]
fn unary_date_subject_interval_hints_pickers() {
    let result = run(
        "[d('2024-01-01')..d('2024-12-31')]",
        true,
        Some(VariableType::Date),
        &VariableType::empty_object(),
    );

    let strs: Vec<_> = result
        .tokens
        .iter()
        .filter(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .collect();
    assert_eq!(strs.len(), 2);
    assert!(strs.iter().all(|t| t.hint == Some(EditHint::DatePicker)));
}

#[test]
fn date_method_arg_hints_picker() {
    let root = obj(&[("startDate", VariableType::Date)]);
    let result = run("startDate.isAfter('2024-01-15')", false, None, &root);

    let date = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .unwrap();
    assert_eq!(date.hint, Some(EditHint::DatePicker));

    let format = run("startDate.format('%Y-%m')", false, None, &root);
    let pattern = format
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .unwrap();
    assert_eq!(pattern.hint, None);
}

#[test]
fn bare_interval_endpoints_hint_picker() {
    let root = obj(&[("startDate", VariableType::Date)]);
    let result = run(
        "startDate in ['2024-01-01'..'2024-12-31']",
        false,
        None,
        &root,
    );

    let strs: Vec<_> = result
        .tokens
        .iter()
        .filter(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .collect();
    assert_eq!(strs.len(), 2);
    assert!(strs.iter().all(|t| t.hint == Some(EditHint::DatePicker)));
}

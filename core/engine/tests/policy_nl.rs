use serde_json::json;
use zen_engine::policy::{
    Cursor, CursorTarget, ExpressionKind, NlExpression, PolicyDocument, PolicyWorkspace,
};
use zen_expression::nl::{EditHint, NlTokenKind, OpSym, TypeTag};

fn workspace() -> PolicyWorkspace {
    let doc: PolicyDocument = serde_json::from_value(json!({
        "blocks": [
            {
                "id": "dm",
                "type": "dataModel",
                "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false },
                        { "id": "p2", "name": "tier", "type": "string", "enum": ["gold", "silver", "bronze"], "array": false, "optional": false }
                    ]
                } },
                "children": []
            },
            {
                "id": "assert1",
                "type": "assertion",
                "props": { "data": {
                    "output": "customer.isAdult",
                    "conditions": [
                        { "id": "c1", "expression": "customer.age >= 18", "operator": "and", "depth": 0 }
                    ]
                } },
                "children": []
            },
            {
                "id": "dt1",
                "type": "decisionTable",
                "props": { "data": {
                    "hitPolicy": "first",
                    "inputs": [
                        { "id": "in1", "name": "Age", "field": "customer.age" },
                        { "id": "in2", "name": "Tier", "field": "customer.tier" }
                    ],
                    "outputs": [ { "id": "out1", "name": "Tag", "field": "customer.tag" } ],
                    "rules": [ { "_id": "row1", "in1": "> 18", "in2": "'gold'", "out1": "'vip'" } ]
                } },
                "children": []
            }
        ]
    }))
    .expect("valid policy fixture");

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("policy", doc);
    ws
}

fn find<'a>(results: &'a [NlExpression], pred: impl Fn(&NlExpression) -> bool) -> &'a NlExpression {
    results
        .iter()
        .find(|e| pred(e))
        .expect("expression present")
}

fn kinds(e: &NlExpression) -> Vec<NlTokenKind> {
    e.result.tokens.iter().map(|t| t.token.clone()).collect()
}

#[test]
fn assertion_condition_resolves_field_type() {
    let ws = workspace();
    let results = ws.nl("policy");

    let condition = find(
        &results,
        |e| matches!(&e.target, CursorTarget::Expression { id } if id.as_ref() == "c1"),
    );

    assert_eq!(condition.kind, ExpressionKind::Standard);
    assert_eq!(condition.block_id.as_ref(), "assert1");
    assert_eq!(
        kinds(condition),
        vec![
            NlTokenKind::Field {
                path: vec!["customer".into(), "age".into()],
                ty: TypeTag::Number,
            },
            NlTokenKind::Op {
                sym: OpSym::Gte,
                implied: false,
                between: false,
            },
            NlTokenKind::Number { value: "18".into() },
        ]
    );
}

#[test]
fn decision_table_input_cell_is_unary_with_resolved_subject() {
    let ws = workspace();
    let results = ws.nl("policy");

    let cell = find(&results, |e| {
        matches!(
            &e.target,
            CursorTarget::DecisionTableCell { row, col }
                if row.as_ref() == "row1" && col.as_ref() == "in1"
        )
    });

    assert_eq!(cell.kind, ExpressionKind::Unary);
    let k = kinds(cell);
    assert_eq!(
        k[0],
        NlTokenKind::Op {
            sym: OpSym::Gt,
            implied: true,
            between: false,
        }
    );
    assert_eq!(k[1], NlTokenKind::Number { value: "18".into() });
}

#[test]
fn decision_table_output_cell_is_standard() {
    let ws = workspace();
    let results = ws.nl("policy");

    let cell = find(&results, |e| {
        matches!(
            &e.target,
            CursorTarget::DecisionTableCell { row, col }
                if row.as_ref() == "row1" && col.as_ref() == "out1"
        )
    });

    assert_eq!(cell.kind, ExpressionKind::Standard);
    assert!(matches!(kinds(cell).as_slice(), [NlTokenKind::Str { .. }]));
}

fn cell_cursor(col: &str) -> Cursor {
    Cursor {
        policy_path: "policy".into(),
        block_id: "dt1".into(),
        pos: 0,
        target: CursorTarget::DecisionTableCell {
            row: "row1".into(),
            col: col.into(),
        },
    }
}

#[test]
fn nl_tokenize_resolves_unary_enum_subject() {
    let ws = workspace();
    let result = ws
        .nl_tokenize(&cell_cursor("in2"), "'gold'")
        .expect("cursor resolves");

    let str_tok = result
        .tokens
        .iter()
        .find(|t| matches!(t.token, NlTokenKind::Str { .. }))
        .expect("string token present");
    assert_eq!(str_tok.hint, Some(EditHint::Select { options: 0 }));
    let labels: Vec<&str> = result.enums[0].iter().map(|o| o.label.as_str()).collect();
    assert_eq!(labels, vec!["gold", "silver", "bronze"]);
    assert!(result.enums[0]
        .iter()
        .all(|o| o.source.as_deref() == Some(format!("\"{}\"", o.label).as_str())));
}

#[test]
fn nl_tokenize_projects_live_text_over_stored_cell() {
    let ws = workspace();
    let result = ws
        .nl_tokenize(&cell_cursor("in1"), "< 21")
        .expect("cursor resolves");

    let tokens: Vec<NlTokenKind> = result.tokens.iter().map(|t| t.token.clone()).collect();
    assert_eq!(
        tokens[0],
        NlTokenKind::Op {
            sym: OpSym::Lt,
            implied: true,
            between: false,
        }
    );
    assert_eq!(tokens[1], NlTokenKind::Number { value: "21".into() });
}

#[test]
fn nl_tokenize_standard_expression_uses_policy_scope() {
    let ws = workspace();
    let cursor = Cursor {
        policy_path: "policy".into(),
        block_id: "assert1".into(),
        pos: 0,
        target: CursorTarget::Expression { id: "c1".into() },
    };
    let result = ws
        .nl_tokenize(&cursor, "customer.age >= 21")
        .expect("cursor resolves");

    let tokens: Vec<NlTokenKind> = result.tokens.iter().map(|t| t.token.clone()).collect();
    assert_eq!(
        tokens[0],
        NlTokenKind::Field {
            path: vec!["customer".into(), "age".into()],
            ty: TypeTag::Number,
        }
    );
}

#[test]
fn decision_table_input_head_is_projected() {
    let ws = workspace();
    let results = ws.nl("policy");

    let head = find(
        &results,
        |e| matches!(&e.target, CursorTarget::DecisionTableHead { col } if col.as_ref() == "in1"),
    );

    assert_eq!(head.kind, ExpressionKind::Standard);
    assert_eq!(
        kinds(head),
        vec![NlTokenKind::Field {
            path: vec!["customer".into(), "age".into()],
            ty: TypeTag::Number,
        }]
    );
}

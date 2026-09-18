use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{
    Cursor, CursorScope, CursorTarget, ExpressionKind, PolicyWorkspace, SlotResponse, SlotRole,
    Workspace,
};
use zen_expression::slot::{LiteralFact, SlotState, ValueOption};
use zen_expression::variable::VariableType;

fn policy_workspace() -> PolicyWorkspace {
    let doc = json!({
        "blocks": [
            {
                "id": "dict1",
                "type": "dictionary",
                "props": { "data": {
                    "name": "status",
                    "entries": [
                        { "id": "e0", "value": "open", "label": "Open case" },
                        { "id": "e1", "value": "closed", "label": "Closed" }
                    ]
                } }
            },
            {
                "id": "dm",
                "type": "dataModel",
                "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false },
                        { "id": "p2", "name": "tier", "type": "string", "enum": ["gold", "silver"], "array": false, "optional": false },
                        { "id": "p3", "name": "name", "type": "string", "array": false, "optional": false },
                        { "id": "p4", "name": "status", "type": "relationship", "target": "status", "array": false, "optional": false },
                        { "id": "p5", "name": "stage", "type": "relationship", "target": "status", "array": false, "optional": false }
                    ]
                } },
                "children": []
            },
            {
                "id": "dt",
                "type": "decisionTable",
                "props": { "data": {
                    "hitPolicy": "first",
                    "inputs": [
                        { "id": "in_declared", "name": "Age", "field": "customer.age" },
                        { "id": "in_dict", "name": "Status", "field": "customer.status" },
                        { "id": "in_stage", "name": "Stage", "field": "customer.stage" },
                        { "id": "in_computed", "name": "Next age", "field": "customer.age + 1" },
                        { "id": "in_condition", "name": "Condition" }
                    ],
                    "outputs": [
                        { "id": "out_dict", "name": "Level", "field": "customer.level", "type": "status" },
                        { "id": "out_number", "name": "Score", "field": "customer.score", "type": "number" },
                        { "id": "out_declared", "name": "Tier", "field": "customer.tier" },
                        { "id": "out_union", "name": "Band", "field": "customer.band" },
                        { "id": "out_untyped", "name": "Label", "field": "customer.label" }
                    ],
                    "rules": [
                        { "_id": "r1", "in_declared": "> 18", "in_dict": "\"open\"", "in_stage": "\"open\"", "in_computed": "> 19", "in_condition": "customer.age > 1", "out_dict": "\"open\"", "out_number": "1", "out_declared": "\"gold\"", "out_union": "\"a\"", "out_untyped": "customer.name" },
                        { "_id": "r2", "in_declared": "", "in_dict": "", "in_stage": "", "in_computed": "", "in_condition": "", "out_dict": "\"closed\"", "out_number": "2", "out_declared": "\"silver\"", "out_union": "\"b\"", "out_untyped": "customer.name" }
                    ]
                } },
                "children": []
            },
            {
                "id": "ex_declared",
                "type": "expression",
                "props": { "data": { "key": "customer.tier", "value": "\"gold\"" } }
            },
            {
                "id": "ex_computed",
                "type": "expression",
                "props": { "data": { "key": "customer.total", "value": "customer.age * 2" } }
            },
            {
                "id": "as1",
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
                "id": "m_declared",
                "type": "match",
                "props": { "data": {
                    "key": "customer.status",
                    "arms": [
                        { "id": "a1", "condition": "customer.age > 50", "value": "\"open\"" },
                        { "id": "a2", "condition": "", "value": "\"closed\"" }
                    ]
                } }
            },
            {
                "id": "m_union",
                "type": "match",
                "props": { "data": {
                    "key": "customer.group",
                    "arms": [
                        { "id": "b1", "condition": "customer.age > 50", "value": "\"senior\"" },
                        { "id": "b2", "condition": "customer.age > 12", "value": "\"teen\"" },
                        { "id": "b3", "condition": "", "value": "\"kid\"" }
                    ]
                } }
            },
            {
                "id": "m_opaque",
                "type": "match",
                "props": { "data": {
                    "key": "customer.alias",
                    "arms": [
                        { "id": "d1", "condition": "customer.age > 50", "value": "customer.name" },
                        { "id": "d2", "condition": "", "value": "\"n/a\"" }
                    ]
                } }
            }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "policy",
        serde_json::from_value(doc).expect("valid policy fixture"),
    );
    ws
}

fn cursor(block: &str, target: CursorTarget) -> Cursor {
    Cursor {
        policy_path: "policy".into(),
        block_id: block.into(),
        pos: 0,
        target,
    }
}

fn cell(row: &str, col: &str) -> CursorTarget {
    CursorTarget::DecisionTableCell {
        row: row.into(),
        col: col.into(),
    }
}

fn head(col: &str) -> CursorTarget {
    CursorTarget::DecisionTableHead { col: col.into() }
}

fn expression(id: &str) -> CursorTarget {
    CursorTarget::Expression { id: id.into() }
}

fn scope_of(ws: &Workspace, cursor: &Cursor) -> CursorScope {
    ws.cursor_scope(cursor)
        .unwrap_or_else(|| panic!("scope for {:?}", cursor.target))
}

fn enum_values(t: &VariableType) -> Vec<String> {
    match t {
        VariableType::Enum(_, values) => {
            let mut out: Vec<String> = values.iter().map(|v| v.to_string()).collect();
            out.sort();
            out
        }
        other => panic!("expected enum, got {other:?}"),
    }
}

fn assert_path(scope: &CursorScope) {
    assert_eq!(scope.kind, ExpressionKind::Standard);
    assert_eq!(scope.role, SlotRole::Path);
    assert!(scope.expected.is_none());
    assert!(scope.subject_type().is_none());
}

fn assert_condition(scope: &CursorScope) {
    assert_eq!(scope.kind, ExpressionKind::Standard);
    assert_eq!(scope.role, SlotRole::Condition);
    assert_eq!(scope.expected, Some(VariableType::Bool));
    assert_eq!(scope.subject_type(), Some(VariableType::Bool));
}

#[test]
fn table_input_cell_with_declared_head_is_unary_over_field_type() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "in_declared")));
    assert_eq!(scope.kind, ExpressionKind::Unary);
    assert_eq!(scope.role, SlotRole::Unary);
    assert_eq!(scope.scope.get("$"), VariableType::Number);
    assert_eq!(scope.subject_type(), Some(VariableType::Number));
    assert!(scope.expected.is_none());
    assert_eq!(scope.scope.get("customer").get("age"), VariableType::Number);
}

#[test]
fn table_input_cell_with_dictionary_head_exposes_enum_subject() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "in_dict")));
    assert_eq!(scope.role, SlotRole::Unary);
    assert_eq!(enum_values(&scope.scope.get("$")), vec!["closed", "open"]);
}

#[test]
fn table_input_cell_with_computed_head_types_subject_by_analysis() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r2", "in_computed")));
    assert_eq!(scope.kind, ExpressionKind::Unary);
    assert_eq!(scope.subject_type(), Some(VariableType::Number));
}

#[test]
fn table_input_cell_without_head_is_a_condition() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "in_condition")));
    assert_condition(&scope);
}

#[test]
fn table_output_cell_uses_declared_dictionary_type() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "out_dict")));
    assert_eq!(scope.kind, ExpressionKind::Standard);
    assert_eq!(scope.role, SlotRole::Value);
    let expected = scope.expected.as_ref().expect("dictionary type");
    assert_eq!(enum_values(expected), vec!["closed", "open"]);
    assert_eq!(scope.subject_type().as_ref(), Some(expected));
}

#[test]
fn table_output_cell_uses_declared_primitive_type() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "out_number")));
    assert_eq!(scope.role, SlotRole::Value);
    assert_eq!(scope.expected, Some(VariableType::Number));
}

#[test]
fn table_output_cell_falls_back_to_declared_field_type() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "out_declared")));
    assert_eq!(scope.role, SlotRole::Value);
    let expected = scope.expected.expect("declared field type");
    assert_eq!(enum_values(&expected), vec!["gold", "silver"]);
}

#[test]
fn table_output_cell_falls_back_to_literal_union_of_cells() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "out_union")));
    let expected = scope.expected.expect("union of cell literals");
    assert_eq!(enum_values(&expected), vec!["a", "b"]);
}

#[test]
fn table_output_cell_without_literal_type_has_no_expectation() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r1", "out_untyped")));
    assert_eq!(scope.role, SlotRole::Value);
    assert!(scope.expected.is_none());
}

#[test]
fn table_cell_scope_does_not_require_the_row_to_exist() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("new-row", "in_declared")));
    assert_eq!(scope.kind, ExpressionKind::Unary);
    assert!(ws
        .cursor_scope(&cursor("dt", cell("r1", "missing-column")))
        .is_none());
}

#[test]
fn table_heads_are_paths() {
    let ws = policy_workspace();
    assert_path(&scope_of(&ws, &cursor("dt", head("in_computed"))));
    assert_path(&scope_of(&ws, &cursor("dt", head("out_dict"))));
    assert!(ws.cursor_scope(&cursor("dt", head("missing"))).is_none());
}

#[test]
fn expression_value_expects_declared_key_type() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("ex_declared", expression("ex_declared")));
    assert_eq!(scope.kind, ExpressionKind::Standard);
    assert_eq!(scope.role, SlotRole::Value);
    let expected = scope.expected.expect("declared tier enum");
    assert_eq!(enum_values(&expected), vec!["gold", "silver"]);
}

#[test]
fn expression_value_with_undeclared_key_has_no_expectation() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("ex_computed", expression("ex_computed")));
    assert_eq!(scope.role, SlotRole::Value);
    assert!(scope.expected.is_none());
}

#[test]
fn expression_key_is_a_path() {
    let ws = policy_workspace();
    assert_path(&scope_of(
        &ws,
        &cursor("ex_declared", CursorTarget::ExpressionKey),
    ));
}

#[test]
fn assertion_condition_is_standard_bool_condition() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("as1", expression("c1")));
    assert_condition(&scope);
    assert_eq!(scope.scope.get("customer").get("age"), VariableType::Number);
}

#[test]
fn assertion_output_is_a_path() {
    let ws = policy_workspace();
    assert_path(&scope_of(
        &ws,
        &cursor("as1", CursorTarget::AssertionOutput),
    ));
}

#[test]
fn assertion_condition_completions_are_standard_not_unary() {
    let ws = policy_workspace();
    let cursor = Cursor {
        policy_path: "policy".into(),
        block_id: "as1".into(),
        pos: "customer.".len() as u32,
        target: expression("c1"),
    };
    let labels: Vec<String> = ws
        .completions(&cursor)
        .into_iter()
        .map(|c| c.label.to_string())
        .collect();
    assert!(labels.iter().any(|l| l == "age"), "{labels:?}");
    let inspect = ws
        .inspect(&Cursor { pos: 0, ..cursor })
        .expect("inspect customer");
    assert!(!matches!(inspect.kind, VariableType::Bool), "{inspect:?}");
}

#[test]
fn match_condition_is_bool_and_target_is_path() {
    let ws = policy_workspace();
    assert_condition(&scope_of(&ws, &cursor("m_declared", expression("a1"))));
    assert_path(&scope_of(
        &ws,
        &cursor("m_declared", CursorTarget::MatchTarget),
    ));
}

#[test]
fn match_value_expects_declared_key_type() {
    let ws = policy_workspace();
    let scope = scope_of(
        &ws,
        &cursor("m_declared", CursorTarget::MatchValue { id: "a1".into() }),
    );
    assert_eq!(scope.role, SlotRole::Value);
    let expected = scope.expected.expect("declared status dictionary");
    assert_eq!(enum_values(&expected), vec!["closed", "open"]);
}

#[test]
fn match_value_with_undeclared_key_uses_sibling_literal_union() {
    let ws = policy_workspace();
    let scope = scope_of(
        &ws,
        &cursor("m_union", CursorTarget::MatchValue { id: "b1".into() }),
    );
    let expected = scope.expected.expect("sibling union");
    assert_eq!(enum_values(&expected), vec!["kid", "teen"]);

    let scope = scope_of(
        &ws,
        &cursor("m_union", CursorTarget::MatchValue { id: "b3".into() }),
    );
    assert_eq!(
        enum_values(&scope.expected.expect("sibling union")),
        vec!["senior", "teen"]
    );
}

#[test]
fn match_value_with_non_literal_siblings_has_no_expectation() {
    let ws = policy_workspace();
    let scope = scope_of(
        &ws,
        &cursor("m_opaque", CursorTarget::MatchValue { id: "d2".into() }),
    );
    assert_eq!(scope.role, SlotRole::Value);
    assert!(scope.expected.is_none());
}

#[test]
fn data_model_targets_and_unknown_blocks_have_no_scope() {
    let ws = policy_workspace();
    assert!(ws
        .cursor_scope(&cursor("dm", CursorTarget::DataModelName))
        .is_none());
    assert!(ws
        .cursor_scope(&cursor(
            "dm",
            CursorTarget::DataModelProperty { id: "p1".into() }
        ))
        .is_none());
    assert!(ws
        .cursor_scope(&cursor("missing", expression("x")))
        .is_none());
    assert!(ws
        .cursor_scope(&cursor("as1", CursorTarget::MatchTarget))
        .is_none());
}

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn edge(id: &str, source: &str, target: &str) -> Value {
    json!({ "id": id, "sourceId": source, "targetId": target, "sourceHandle": null })
}

fn graph_workspace() -> Workspace {
    let mut ws = Workspace::new();
    ws.set_document(
        "dicts",
        document(json!({
            "imports": [],
            "blocks": [{
                "id": "dict1",
                "type": "dictionary",
                "props": { "data": {
                    "name": "customerTier",
                    "entries": [
                        { "id": "e0", "value": "VIP", "label": "Very important" },
                        { "id": "e1", "value": "STD", "label": "Standard" }
                    ]
                } }
            }]
        })),
    );
    let input_schema = json!({
        "type": "object",
        "properties": { "age": { "type": "number" }, "name": { "type": "string" } },
        "required": ["age", "name"]
    });
    let output_schema = json!({
        "type": "object",
        "properties": { "total": { "type": "number" }, "vip": { "type": "boolean" } },
        "required": ["total", "vip"]
    });
    ws.set_document(
        "g",
        document(json!({
            "imports": ["dicts"],
            "nodes": [
                node("in", "inputNode", json!({ "schema": input_schema.to_string() })),
                node("calc", "expressionNode", json!({ "passThrough": true, "expressions": [
                    { "id": "row_total", "key": "total", "value": "age * 2" },
                    { "id": "row_vip", "key": "vip", "value": "age > 60" },
                    { "id": "row_extra", "key": "extra", "value": "name" }
                ] })),
                node("sw", "switchNode", json!({ "statements": [
                    { "id": "s1", "condition": "age > 18" }
                ] })),
                node("dt", "decisionTableNode", json!({
                    "hitPolicy": "first",
                    "inputs": [
                        { "id": "c_field", "name": "Age", "field": "age" },
                        { "id": "c_computed", "name": "Next", "field": "age + 1" },
                        { "id": "c_condition", "name": "Condition" }
                    ],
                    "outputs": [
                        { "id": "o_dict", "name": "Tier", "field": "tier", "type": "customerTier" },
                        { "id": "o_plain", "name": "Score", "field": "score" }
                    ],
                    "rules": [
                        { "_id": "r1", "c_field": "> 18", "c_computed": "> 19", "c_condition": "age > 1", "o_dict": "'VIP'", "o_plain": "1" },
                        { "_id": "r2", "c_field": "", "c_computed": "", "c_condition": "", "o_dict": "'STD'", "o_plain": "2" }
                    ]
                })),
                node("out", "outputNode", json!({ "schema": output_schema.to_string() }))
            ],
            "edges": [
                edge("e1", "in", "calc"),
                edge("e2", "calc", "sw"),
                edge("e3", "sw", "dt"),
                edge("e4", "dt", "out")
            ]
        })),
    );
    ws
}

fn graph_cursor(node: &str, target: CursorTarget) -> Cursor {
    Cursor {
        policy_path: "g".into(),
        block_id: node.into(),
        pos: 0,
        target,
    }
}

#[test]
fn graph_expression_row_is_value_with_output_schema_type() {
    let ws = graph_workspace();
    let scope = scope_of(&ws, &graph_cursor("calc", expression("row_total")));
    assert_eq!(scope.kind, ExpressionKind::Standard);
    assert_eq!(scope.role, SlotRole::Value);
    assert_eq!(scope.expected, Some(VariableType::Number));
    assert_eq!(scope.scope.get("age"), VariableType::Number);
    assert!(!matches!(scope.scope.get("$"), VariableType::Null));
    assert!(!matches!(scope.scope.get("$nodes"), VariableType::Null));

    let scope = scope_of(&ws, &graph_cursor("calc", expression("row_vip")));
    assert_eq!(scope.expected, Some(VariableType::Bool));

    let scope = scope_of(&ws, &graph_cursor("calc", expression("row_extra")));
    assert!(scope.expected.is_none());
}

#[test]
fn graph_switch_statement_is_a_condition() {
    let ws = graph_workspace();
    let scope = scope_of(&ws, &graph_cursor("sw", expression("s1")));
    assert_condition(&scope);
    assert_eq!(scope.scope.get("age"), VariableType::Number);
}

#[test]
fn graph_table_cells_resolve_like_policy_tables() {
    let ws = graph_workspace();
    let scope = scope_of(&ws, &graph_cursor("dt", cell("r1", "c_field")));
    assert_eq!(scope.kind, ExpressionKind::Unary);
    assert_eq!(scope.role, SlotRole::Unary);
    assert_eq!(scope.subject_type(), Some(VariableType::Number));

    let scope = scope_of(&ws, &graph_cursor("dt", cell("r1", "c_computed")));
    assert_eq!(scope.subject_type(), Some(VariableType::Number));

    assert_condition(&scope_of(
        &ws,
        &graph_cursor("dt", cell("r1", "c_condition")),
    ));

    let scope = scope_of(&ws, &graph_cursor("dt", cell("r1", "o_dict")));
    assert_eq!(scope.role, SlotRole::Value);
    assert_eq!(
        enum_values(&scope.expected.expect("dictionary column")),
        vec!["STD", "VIP"]
    );

    let scope = scope_of(&ws, &graph_cursor("dt", cell("r2", "o_plain")));
    assert_eq!(scope.role, SlotRole::Value);
    assert!(scope.expected.is_none());
}

#[test]
fn graph_table_head_and_transform_input_are_paths() {
    let ws = graph_workspace();
    assert_path(&scope_of(&ws, &graph_cursor("dt", head("c_computed"))));
    assert_path(&scope_of(&ws, &graph_cursor("dt", head("o_dict"))));
    let scope = scope_of(&ws, &graph_cursor("calc", CursorTarget::TransformInput));
    assert_path(&scope);
    assert_eq!(scope.scope.get("age"), VariableType::Number);
    assert!(ws
        .cursor_scope(&graph_cursor("sw", CursorTarget::TransformInput))
        .is_none());
    assert!(ws
        .cursor_scope(&graph_cursor("in", expression("x")))
        .is_none());
}

fn slot_at(ws: &Workspace, block: &str, target: CursorTarget, text: &str) -> SlotResponse {
    let mut cursor = cursor(block, target);
    cursor.pos = text.encode_utf16().count() as u32;
    ws.slot(&cursor, text).expect("slot resolves")
}

fn labels(options: &[ValueOption]) -> Vec<&str> {
    options.iter().map(|o| o.label.as_str()).collect()
}

#[test]
fn slot_table_unary_cell_uses_dictionary_labels() {
    let ws = policy_workspace();
    let response = slot_at(&ws, "dt", cell("r1", "in_stage"), "== \"");
    assert_eq!(response.kind, ExpressionKind::Unary);
    assert_eq!(response.role, SlotRole::Unary);
    assert_eq!(response.slot.state, SlotState::InString);
    assert_eq!(response.slot.replace_span, (3, 4));
    assert_eq!(labels(&response.slot.options), vec!["Open case", "Closed"]);
    assert!(response.slot.auto_open);
    assert_eq!(
        enum_values(&response.subject_type.expect("unary subject")),
        vec!["closed", "open"]
    );
    assert!(response.expected_type.is_none());
}

#[test]
fn slot_match_value_offers_declared_enum_options() {
    let ws = policy_workspace();
    let response = slot_at(
        &ws,
        "m_declared",
        CursorTarget::MatchValue { id: "a1".into() },
        "",
    );
    assert_eq!(response.kind, ExpressionKind::Standard);
    assert_eq!(response.role, SlotRole::Value);
    assert_eq!(response.slot.state, SlotState::Value);
    assert_eq!(labels(&response.slot.options), vec!["Open case", "Closed"]);
    assert_eq!(
        enum_values(&response.expected_type.expect("declared key type")),
        vec!["closed", "open"]
    );
}

#[test]
fn slot_assertion_condition_expects_a_number_after_comparison() {
    let ws = policy_workspace();
    let response = slot_at(&ws, "as1", expression("c1"), "customer.age >= ");
    assert_eq!(response.kind, ExpressionKind::Standard);
    assert_eq!(response.role, SlotRole::Condition);
    assert_eq!(response.expected_type, Some(VariableType::Bool));
    assert_eq!(response.slot.state, SlotState::Value);
    assert_eq!(response.slot.expected, Some(VariableType::Number));
    assert_eq!(response.slot.replace_span, (16, 16));
}

#[test]
fn slot_graph_switch_statement_is_a_condition() {
    let ws = graph_workspace();
    let mut cursor = graph_cursor("sw", expression("s1"));
    let text = "age > ";
    cursor.pos = text.len() as u32;
    let response = ws.slot(&cursor, text).expect("graph slot");
    assert_eq!(response.role, SlotRole::Condition);
    assert_eq!(response.slot.state, SlotState::Value);
    assert_eq!(response.slot.expected, Some(VariableType::Number));
    assert_eq!(response.slot.replace_span, (6, 6));
}

#[test]
fn slot_spans_are_utf16_code_units() {
    let ws = policy_workspace();
    let text = "customer.name == \"😀東京\" and customer.stage == \"";
    let units = text.encode_utf16().count() as u32;
    assert_eq!(units, 47);
    assert_eq!(text.len(), 53);
    let response = slot_at(&ws, "dt", cell("r1", "in_condition"), text);
    assert_eq!(response.slot.state, SlotState::InString);
    assert_eq!(response.slot.replace_span, (units - 1, units));
    assert_eq!(labels(&response.slot.options), vec!["Open case", "Closed"]);

    let text = "customer.name == \"😀\" and customer.stage == \"open\"";
    let response = slot_at(&ws, "dt", cell("r1", "in_condition"), text);
    let fact = response
        .literals
        .iter()
        .find_map(|f| match f {
            LiteralFact::Enum { span, label, .. } => Some((*span, label.clone())),
            _ => None,
        })
        .expect("enum literal");
    assert_eq!(fact, ((44, 50), "Open case".to_string()));
}

#[test]
fn slot_is_none_outside_expression_targets() {
    let ws = policy_workspace();
    assert!(ws
        .slot(&cursor("dm", CursorTarget::DataModelName), "x")
        .is_none());
    assert!(ws.slot(&cursor("missing", expression("x")), "x").is_none());
}

#[test]
fn facts_match_per_target_slot_literals_and_carry_labels() {
    let ws = policy_workspace();
    let facts = ws.facts("policy");
    assert_eq!(facts.len(), 41);
    for entry in &facts {
        let response = ws
            .slot(
                &cursor(&entry.block_id, entry.target.clone()),
                &entry.source,
            )
            .expect("every fact target resolves");
        assert_eq!(response.kind, entry.kind);
        assert_eq!(response.role, entry.role);
        assert_eq!(response.literals, entry.literals, "{:?}", entry.target);
        assert_eq!(response.enums, entry.enums, "{:?}", entry.target);
    }

    let find = |block: &str, target: CursorTarget| {
        let wanted = serde_json::to_value(&target).unwrap();
        facts
            .iter()
            .find(|f| {
                f.block_id.as_ref() == block && serde_json::to_value(&f.target).unwrap() == wanted
            })
            .expect("fact present")
    };
    let open = find("dt", cell("r1", "in_stage"));
    assert_eq!(open.kind, ExpressionKind::Unary);
    assert!(matches!(
        &open.literals[..],
        [LiteralFact::Enum { label, valid: true, span: (0, 6), .. }] if label == "Open case"
    ));
    assert_eq!(labels(&open.subject_options), vec!["Open case", "Closed"]);

    let empty = find("dt", cell("r2", "in_stage"));
    assert!(empty.source.is_empty());
    assert!(empty.literals.is_empty());
    assert_eq!(labels(&empty.subject_options), vec!["Open case", "Closed"]);

    let closed = find("dt", cell("r2", "out_dict"));
    assert_eq!(closed.role, SlotRole::Value);
    assert!(matches!(
        &closed.literals[..],
        [LiteralFact::Enum { label, .. }] if label == "Closed"
    ));

    let head = find("dt", head("in_stage"));
    assert_eq!(head.role, SlotRole::Path);
    assert!(head.literals.is_empty());

    let arm = find("m_declared", CursorTarget::MatchValue { id: "a2".into() });
    assert!(matches!(&arm.literals[..], [LiteralFact::Enum { label, .. }] if label == "Closed"));
    assert_eq!(labels(&arm.subject_options), vec!["Open case", "Closed"]);
}

#[test]
fn graph_facts_carry_imported_dictionary_labels() {
    let ws = graph_workspace();
    let facts = ws.facts("g");
    assert!(!facts.is_empty());
    let vip = facts
        .iter()
        .find(|f| {
            f.block_id.as_ref() == "dt"
                && matches!(&f.target, CursorTarget::DecisionTableCell { row, col } if row.as_ref() == "r1" && col.as_ref() == "o_dict")
        })
        .expect("o_dict cell");
    assert_eq!(vip.role, SlotRole::Value);
    assert!(matches!(
        &vip.literals[..],
        [LiteralFact::Enum { label, valid: true, .. }] if label == "Very important"
    ));
    assert_eq!(
        labels(&vip.subject_options),
        vec!["Very important", "Standard"]
    );

    let total = facts
        .iter()
        .find(|f| f.block_id.as_ref() == "calc" && matches!(&f.target, CursorTarget::Expression { id } if id.as_ref() == "row_total"))
        .expect("expression row");
    assert_eq!(total.expected_type, Some(VariableType::Number));
    assert!(ws.facts("nope").is_empty());
}

#[test]
fn facts_cover_every_assertion_condition_with_exact_source() {
    let doc = json!({
        "blocks": [
            {
                "id": "dict1",
                "type": "dictionary",
                "props": { "data": {
                    "name": "status",
                    "entries": [
                        { "id": "e0", "value": "aog", "label": "Aircraft on ground" },
                        { "id": "e1", "value": "ok", "label": "Serviceable" }
                    ]
                } }
            },
            {
                "id": "dm",
                "type": "dataModel",
                "props": { "data": {
                    "name": "grounding",
                    "properties": [
                        { "id": "p1", "name": "status", "type": "relationship", "target": "status", "array": false, "optional": false },
                        { "id": "p2", "name": "hours", "type": "number", "array": false, "optional": false }
                    ]
                } },
                "children": []
            },
            {
                "id": "as_two",
                "type": "assertion",
                "props": { "data": {
                    "output": "grounding.blocked",
                    "conditions": [
                        { "id": "cond_a", "expression": " grounding.status == \"aog\" ", "operator": "and", "depth": 0 },
                        { "id": "cond_b", "expression": "grounding.hours > 4 ", "operator": "and", "depth": 0 }
                    ]
                } },
                "children": []
            }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("policy", serde_json::from_value(doc).expect("valid policy fixture"));
    let facts = ws.facts("policy");
    let find = |id: &str| {
        facts
            .iter()
            .find(|f| {
                f.block_id.as_ref() == "as_two"
                    && matches!(&f.target, CursorTarget::Expression { id: got } if got.as_ref() == id)
            })
            .unwrap_or_else(|| panic!("facts entry for condition {id}"))
    };
    let first = find("cond_a");
    assert_eq!(first.source.as_ref(), " grounding.status == \"aog\" ");
    assert_eq!(first.role, SlotRole::Condition);
    assert!(matches!(
        &first.literals[..],
        [LiteralFact::Enum { label, valid: true, span: (21, 26), .. }] if label == "Aircraft on ground"
    ));
    let second = find("cond_b");
    assert_eq!(second.source.as_ref(), "grounding.hours > 4 ");
    assert!(second.literals.is_empty());
}

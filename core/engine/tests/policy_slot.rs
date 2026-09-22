use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{
    Cursor, CursorScope, CursorTarget, DiagnosticCode, ExpressionKind, PolicyWorkspace,
    SlotResponse, SlotRole, Workspace,
};
use zen_expression::intellisense::completion::CompletionKind;
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
                        { "id": "out_untyped", "name": "Label", "field": "customer.label" },
                        { "id": "out_nofield", "name": "Unnamed", "field": "" },
                        { "id": "out_expr", "name": "Computed", "field": "customer.age + 1" }
                    ],
                    "rules": [
                        { "_id": "r1", "in_declared": "> 18", "in_dict": "\"open\"", "in_stage": "\"open\"", "in_computed": "> 19", "in_condition": "customer.age > 1", "out_dict": "\"open\"", "out_number": "1", "out_declared": "\"gold\"", "out_union": "\"a\"", "out_untyped": "customer.name", "out_nofield": "\"x\"", "out_expr": "10" },
                        { "_id": "r2", "in_declared": "", "in_dict": "", "in_stage": "", "in_computed": "", "in_condition": "", "out_dict": "\"closed\"", "out_number": "2", "out_declared": "\"silver\"", "out_union": "\"b\"", "out_untyped": "customer.name", "out_nofield": "\"y\"", "out_expr": "20" },
                        { "_id": "r3", "in_declared": "", "in_dict": "", "in_stage": "", "in_computed": "", "in_condition": "", "out_dict": "", "out_number": "", "out_declared": "", "out_union": "", "out_untyped": "", "out_nofield": "", "out_expr": "" }
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

#[test]
fn unicode_completions_and_nested_member_receivers_use_slot_context() {
    for source in [
        "\"é\" == customer.",
        "\"😀\" == customer.",
        "  \"😀\" == customer.",
        "customer.age in [customer.",
    ] {
        let mut ws = Workspace::new();
        ws.set_policy("policy", serde_json::from_value(json!({"blocks":[
            {"id":"dm", "type":"dataModel", "props":{"data":{"name":"customer", "properties":[
                {"id":"name", "name":"name", "type":"string", "array":false, "optional":false},
                {"id":"age", "name":"age", "type":"number", "array":false, "optional":false}
            ]}}},
            {"id":"ex_computed", "type":"expression", "props":{"data":{"key":"result", "value":source}}}
        ]})).unwrap());
        let mut at = cursor("ex_computed", expression("ex_computed"));
        at.pos = source.encode_utf16().count() as u32;
        let items = ws.completions(&at);
        let expected = if source.starts_with("customer.age in") {
            "age"
        } else {
            "name"
        };
        assert!(
            items.iter().any(|item| item.label == expected),
            "{source}: {items:?}"
        );
        assert!(!items.iter().any(|item| item.label == "year"), "{source}");
    }
}

#[test]
fn scope_before_isolates_nested_writes_and_the_declared_schema() {
    let mut ws = Workspace::new();
    ws.set_policy("policy", serde_json::from_value(json!({"blocks":[
        {"id":"dm", "type":"dataModel", "props":{"data":{"name":"customer", "properties":[
            {"id":"name", "name":"name", "type":"string", "optional":false, "array":false}
        ]}}},
        {"id":"a", "type":"expression", "props":{"data":{"key":"customer.first", "value":"1"}}},
        {"id":"b", "type":"expression", "props":{"data":{"key":"customer.risk", "value":"\"high\""}}},
        {"id":"c", "type":"expression", "props":{"data":{"key":"customer.last", "value":"2"}}}
    ]})).unwrap());
    for block in ["a", "b", "c", "a"] {
        let response = slot_at(&ws, block, expression(block), "customer.risk == ");
        assert_eq!(
            response.slot.options.iter().any(|o| o.value == "high"),
            block == "c",
            "{block}"
        );
    }
}

#[test]
fn optional_outputs_and_collect_cells_keep_their_actual_value_type() {
    let mut ws = Workspace::new();
    ws.set_policy("policy", serde_json::from_value(json!({"blocks":[
        {"id":"dm", "type":"dataModel", "props":{"data":{"name":"customer", "properties":[
            {"id":"stage", "name":"stage", "type":"string", "enum":["open","closed"], "optional":true, "array":false},
            {"id":"tags", "name":"tags", "type":"string", "enum":["open","closed"], "optional":false, "array":true}
        ]}}},
        {"id":"e", "type":"expression", "props":{"data":{"key":"customer.stage", "value":""}}},
        {"id":"dt", "type":"decisionTable", "props":{"data":{"hitPolicy":"collect", "inputs":[],
            "outputs":[{"id":"out", "name":"Tags", "field":"customer.tags"}], "rules":[{"_id":"r", "out":""}]}}}
    ]})).unwrap());
    let optional = slot_at(&ws, "e", expression("e"), "");
    assert!(optional
        .slot
        .options
        .iter()
        .any(|o| o.source.as_deref() == Some("null")));
    let collect = slot_at(&ws, "dt", cell("r", "out"), "");
    assert!(matches!(
        collect.expected_type,
        Some(VariableType::Enum(..))
    ));
    assert_eq!(collect.slot.options[0].source.as_deref(), Some("\"open\""));
}

#[test]
fn graph_output_path_locates_nested_schema_and_bulk_facts_exclude_active_rows() {
    let mut ws = Workspace::new();
    let schema = json!({"type":"object", "required":["result"], "properties":{"result":{
        "type":"object", "required":["status"], "properties":{"status":{"type":"string", "enum":["open","closed"]}}
    }}});
    ws.set_document("g", document(json!({"nodes":[
        node("in", "inputNode", json!({})),
        node("calc", "expressionNode", json!({"outputPath":"result", "expressions":[{"id":"e", "key":"status", "value":""}]})),
        node("out", "outputNode", json!({"schema":schema.to_string()}))
    ], "edges":[edge("a","in","calc"), edge("b","calc","out")]})));
    let slot = slot_at_graph(&ws, "calc", expression("e"), "");
    assert_eq!(slot.slot.options.len(), 2);

    let ws = graph_workspace();
    for fact in ws.facts("g") {
        if fact.block_id.as_ref() != "dt_untyped" {
            continue;
        }
        let at = graph_cursor("dt_untyped", fact.target.clone());
        let live = ws.slot(&at, &fact.source).unwrap();
        assert_eq!(fact.expected_type, live.expected_type);
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
fn table_output_cell_without_path_uses_sibling_literal_union() {
    let ws = policy_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r3", "out_nofield")));
    assert_eq!(scope.role, SlotRole::Value);
    let expected = scope.expected.expect("union of sibling literals");
    assert_eq!(enum_values(&expected), vec!["x", "y"]);

    let scope = scope_of(&ws, &cursor("dt", cell("r1", "out_nofield")));
    assert_eq!(scope.expected, Some(VariableType::Const("y".into())));

    let scope = scope_of(&ws, &cursor("dt", cell("r3", "out_expr")));
    assert_eq!(scope.expected, Some(VariableType::Number));
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
        "properties": {
            "total": { "type": "number" },
            "vip": { "type": "boolean" },
            "level": { "$dictionary": "customerTier" }
        },
        "required": ["total", "vip", "level"]
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
                        { "id": "o_plain", "name": "Score", "field": "score" },
                        { "id": "o_schema", "name": "Total", "field": "total" },
                        { "id": "o_schema_dict", "name": "Level", "field": "level" }
                    ],
                    "rules": [
                        { "_id": "r1", "c_field": "> 18", "c_computed": "> 19", "c_condition": "age > 1", "o_dict": "'VIP'", "o_plain": "1", "o_schema": "", "o_schema_dict": "" },
                        { "_id": "r2", "c_field": "", "c_computed": "", "c_condition": "", "o_dict": "'STD'", "o_plain": "2", "o_schema": "", "o_schema_dict": "" }
                    ]
                })),
                node("dt_untyped", "decisionTableNode", json!({
                    "hitPolicy": "first",
                    "inputs": [{ "id": "u_age", "name": "Age", "field": "age" }],
                    "outputs": [
                        { "id": "o_bool", "name": "Triggered", "field": "triggered" },
                        { "id": "o_str", "name": "Bucket", "field": "bucket" },
                        { "id": "o_mixed", "name": "Mixed", "field": "mixed" }
                    ],
                    "rules": [
                        { "_id": "u1", "u_age": "> 18", "o_bool": "false", "o_str": "'a'", "o_mixed": "true" },
                        { "_id": "u2", "u_age": "> 60", "o_bool": "true", "o_str": "'b'", "o_mixed": "'x'" },
                        { "_id": "u3", "u_age": "", "o_bool": "", "o_str": "", "o_mixed": "" }
                    ]
                })),
                node("out", "outputNode", json!({ "schema": output_schema.to_string() }))
            ],
            "edges": [
                edge("e1", "in", "calc"),
                edge("e2", "calc", "sw"),
                edge("e3", "sw", "dt"),
                edge("e4", "dt", "out"),
                edge("e5", "sw", "dt_untyped"),
                edge("e6", "dt_untyped", "out")
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
    assert_eq!(scope.expected, Some(VariableType::Number));
}

#[test]
fn graph_table_undeclared_output_cell_uses_sibling_literal_union() {
    let ws = graph_workspace();
    let scope = scope_of(&ws, &graph_cursor("dt_untyped", cell("u3", "o_bool")));
    assert_eq!(scope.role, SlotRole::Value);
    assert_eq!(scope.expected, Some(VariableType::Bool));

    let scope = scope_of(&ws, &graph_cursor("dt_untyped", cell("u3", "o_str")));
    assert_eq!(
        enum_values(&scope.expected.expect("union of sibling string literals")),
        vec!["a", "b"]
    );

    let scope = scope_of(&ws, &graph_cursor("dt_untyped", cell("u1", "o_str")));
    assert_eq!(scope.expected, Some(VariableType::Const("b".into())));

    let scope = scope_of(&ws, &graph_cursor("dt_untyped", cell("u3", "o_mixed")));
    assert_eq!(scope.role, SlotRole::Value);
    assert!(scope.expected.is_none());
}

#[test]
fn graph_table_undeclared_output_column_uses_output_schema_type() {
    let ws = graph_workspace();
    let scope = scope_of(&ws, &graph_cursor("dt", cell("r1", "o_schema")));
    assert_eq!(scope.role, SlotRole::Value);
    assert_eq!(scope.expected, Some(VariableType::Number));

    let scope = scope_of(&ws, &graph_cursor("dt", cell("r1", "o_schema_dict")));
    let expected = scope.expected.expect("dictionary from output schema");
    assert!(
        matches!(&expected, VariableType::Enum(Some(name), _) if name.as_ref() == "customerTier")
    );
    assert_eq!(enum_values(&expected), vec!["STD", "VIP"]);

    let response = slot_at_graph(&ws, "dt", cell("r1", "o_schema_dict"), "");
    assert_eq!(response.slot.state, SlotState::Value);
    assert_eq!(
        labels(&response.slot.options),
        vec!["Very important", "Standard"]
    );
}

#[test]
fn graph_expression_key_is_a_path_over_node_input() {
    let ws = graph_workspace();
    let scope = scope_of(&ws, &graph_cursor("calc", CursorTarget::ExpressionKey));
    assert_path(&scope);
    assert_eq!(scope.scope.get("age"), VariableType::Number);

    let response = slot_at_graph(&ws, "calc", CursorTarget::ExpressionKey, "");
    assert_eq!(response.role, SlotRole::Path);
    assert_eq!(response.slot.state, SlotState::Path);
    assert!(response.slot.auto_open);

    let labels: Vec<String> = ws
        .completions(&graph_cursor("calc", CursorTarget::ExpressionKey))
        .into_iter()
        .map(|c| c.label.to_string())
        .collect();
    assert!(labels.iter().any(|l| l == "age"), "{labels:?}");
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

fn slot_at_graph(ws: &Workspace, node: &str, target: CursorTarget, text: &str) -> SlotResponse {
    let mut cursor = graph_cursor(node, target);
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
fn slot_graph_undeclared_output_cell_offers_sibling_literals() {
    let ws = graph_workspace();
    let mut cursor = graph_cursor("dt_untyped", cell("u3", "o_str"));
    cursor.pos = 0;
    let response = ws.slot(&cursor, "").expect("graph slot");
    assert_eq!(response.kind, ExpressionKind::Standard);
    assert_eq!(response.role, SlotRole::Value);
    assert_eq!(response.slot.state, SlotState::Value);
    assert_eq!(labels(&response.slot.options), vec!["a", "b"]);
    assert_eq!(
        enum_values(&response.expected_type.expect("sibling union")),
        vec!["a", "b"]
    );

    let cursor = graph_cursor("dt_untyped", cell("u3", "o_bool"));
    let response = ws.slot(&cursor, "").expect("graph slot");
    assert_eq!(response.expected_type, Some(VariableType::Bool));
    assert_eq!(response.slot.expected, Some(VariableType::Bool));
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
    assert_eq!(facts.len(), 57);
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
    ws.set_policy(
        "policy",
        serde_json::from_value(doc).expect("valid policy fixture"),
    );
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

fn cyclic_entity_workspace() -> PolicyWorkspace {
    let doc = json!({
        "blocks": [
            {
                "id": "dm_item",
                "type": "dataModel",
                "props": { "data": {
                    "name": "lineItem",
                    "properties": [
                        { "id": "l1", "name": "qty", "type": "number", "array": false, "optional": false },
                        { "id": "l2", "name": "claim", "type": "relationship", "target": "claim", "array": false, "optional": false }
                    ]
                } },
                "children": []
            },
            {
                "id": "dm_addr",
                "type": "dataModel",
                "props": { "data": {
                    "name": "address",
                    "properties": [
                        { "id": "a1", "name": "city", "type": "string", "array": false, "optional": false },
                        { "id": "a2", "name": "claim", "type": "relationship", "target": "claim", "array": false, "optional": false }
                    ]
                } },
                "children": []
            },
            {
                "id": "dm",
                "type": "dataModel",
                "props": { "data": {
                    "name": "claim",
                    "properties": [
                        { "id": "p1", "name": "amount", "type": "number", "array": false, "optional": false },
                        { "id": "p2", "name": "address", "type": "relationship", "target": "address", "array": false, "optional": false },
                        { "id": "p3", "name": "items", "type": "relationship", "target": "lineItem", "array": true, "optional": false }
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
                        { "id": "c_items", "name": "Items", "field": "claim.items" },
                        { "id": "c_addr", "name": "Address", "field": "claim.address" },
                        { "id": "c_cond", "name": "Condition" }
                    ],
                    "outputs": [{ "id": "o", "name": "Out", "field": "claim.o" }],
                    "rules": [{ "_id": "r1", "c_items": "", "c_addr": "", "c_cond": "", "o": "" }]
                } },
                "children": []
            },
            {
                "id": "ex",
                "type": "expression",
                "props": { "data": { "key": "claim.copy", "value": "claim.amount" } }
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

fn assert_finite_json<T: serde::Serialize>(value: &T) -> String {
    let json = serde_json::to_string(value).expect("serialisable response");
    assert!(
        json.len() < 64 * 1024,
        "type graph was not cut: {} bytes",
        json.len()
    );
    json
}

#[test]
fn slot_survives_mutually_referencing_entities() {
    let ws = cyclic_entity_workspace();
    let mut cursor = cursor("dt", cell("r1", "c_items"));
    let response = ws.slot(&cursor, "").expect("unary slot over entity array");
    assert_eq!(response.role, SlotRole::Unary);
    assert!(matches!(
        response.subject_type,
        Some(VariableType::Array(_))
    ));
    assert_eq!(response.slot.state, SlotState::UnaryStart);
    assert_finite_json(&response);

    let text = "some(#, #.qty > 1)";
    cursor.pos = text.len() as u32;
    let response = ws.slot(&cursor, text).expect("closure over entity array");
    assert_finite_json(&response);

    let cursor = cursor_at("dt", cell("r1", "c_addr"), 0);
    let response = ws.slot(&cursor, "").expect("unary slot over entity object");
    assert!(matches!(
        response.subject_type,
        Some(VariableType::Object(_))
    ));
    assert_finite_json(&response);

    for text in [
        "claim.address == ",
        "claim == ",
        "claim.items ",
        "claim.address.city",
    ] {
        let cursor = cursor_at("dt", cell("r1", "c_cond"), text.len() as u32);
        let response = ws.slot(&cursor, text).expect(text);
        assert_eq!(response.role, SlotRole::Condition);
        assert_finite_json(&response);
    }

    let cursor = cursor_at("dt", cell("r1", "c_cond"), "claim.address == ".len() as u32);
    let response = ws
        .slot(&cursor, "claim.address == ")
        .expect("object comparison");
    assert_eq!(response.slot.state, SlotState::Value);
    assert!(matches!(
        response.slot.expected,
        Some(VariableType::Object(_))
    ));
    let json = assert_finite_json(&response);
    assert!(
        json.contains("\"city\""),
        "entity fields survive the cut: {json}"
    );
}

#[test]
fn inspect_and_completions_survive_mutually_referencing_entities() {
    let ws = cyclic_entity_workspace();
    let cursor = cursor_at("ex", expression("ex"), 2);
    let result = ws.inspect(&cursor).expect("inspect on entity root");
    assert!(matches!(result.kind, VariableType::Object(_)));
    assert_finite_json(&result);

    let cursor = cursor_at("dt", cell("r1", "c_cond"), 0);
    let completions = ws.completions(&cursor);
    assert!(completions.iter().any(|c| c.label == "claim"));
    assert_finite_json(&completions);

    let facts = ws.facts("policy");
    assert!(facts.iter().any(|f| f.block_id.as_ref() == "dt"));
    assert_finite_json(&facts);
}

fn cursor_at(block: &str, target: CursorTarget, pos: u32) -> Cursor {
    let mut cursor = cursor(block, target);
    cursor.pos = pos;
    cursor
}

fn writer_workspace() -> PolicyWorkspace {
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
                    "name": "claim",
                    "properties": [
                        { "id": "p1", "name": "amount", "type": "number", "array": false, "optional": false },
                        { "id": "p2", "name": "kind", "type": "string", "enum": ["auto", "home"], "array": false, "optional": false },
                        { "id": "p3", "name": "status", "type": "relationship", "target": "status", "array": false, "optional": false },
                        { "id": "p4", "name": "filedAt", "type": "date", "array": false, "optional": false }
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
                        { "id": "in_dict", "name": "Status", "field": "claim.status" },
                        { "id": "in_enum", "name": "Kind", "field": "claim.kind" },
                        { "id": "in_date", "name": "Filed", "field": "claim.filedAt" },
                        { "id": "in_total", "name": "Total", "field": "claim.total" }
                    ],
                    "outputs": [
                        { "id": "out_declared_date", "name": "FiledAt", "field": "claim.filedAt" },
                        { "id": "out_band", "name": "Band", "field": "claim.band" }
                    ],
                    "rules": [
                        { "_id": "r1", "in_dict": "\"open\"", "in_enum": "\"auto\"", "in_date": "", "in_total": "", "out_declared_date": "", "out_band": "\"a\"" },
                        { "_id": "r2", "in_dict": "", "in_enum": "", "in_date": "", "in_total": "", "out_declared_date": "", "out_band": "\"b\"" }
                    ]
                } },
                "children": []
            },
            { "id": "ex_kind", "type": "expression", "props": { "data": { "key": "claim.kind", "value": "\"auto\"" } } },
            { "id": "ex_total", "type": "expression", "props": { "data": { "key": "claim.total", "value": "claim.amount * 2" } } },
            {
                "id": "m_status",
                "type": "match",
                "props": { "data": {
                    "key": "claim.status",
                    "arms": [
                        { "id": "a1", "condition": "claim.amount > 50", "value": "\"open\"" },
                        { "id": "a2", "condition": "", "value": "\"closed\"" }
                    ]
                } }
            },
            {
                "id": "as_empty",
                "type": "assertion",
                "props": { "data": {
                    "output": "claim.isBig",
                    "conditions": [
                        { "id": "c1", "expression": "claim.amount > 100", "operator": "and", "depth": 0 },
                        { "id": "c2", "expression": "", "operator": "and", "depth": 0 }
                    ]
                } },
                "children": []
            }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "policy",
        serde_json::from_value(doc).expect("valid writer fixture"),
    );
    ws
}

#[test]
fn dictionary_cell_keeps_labels_with_match_writer() {
    let ws = writer_workspace();
    let response = slot_at(&ws, "dt", cell("r2", "in_dict"), "== \"");
    assert_eq!(labels(&response.slot.options), vec!["Open case", "Closed"]);
    match response.subject_type.expect("unary subject") {
        VariableType::Enum(name, _) => assert_eq!(name.as_deref(), Some("status")),
        other => panic!("expected dictionary enum, got {other:?}"),
    }
    let diagnostics = ws.diagnostics("policy");
    assert!(diagnostics
        .iter()
        .any(|d| d.location.block_id.as_deref() == Some("m_status")
            && matches!(d.code, DiagnosticCode::InputOverride)));
}

#[test]
fn inline_enum_cell_keeps_members_with_expression_writer() {
    let ws = writer_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r2", "in_enum")));
    assert_eq!(enum_values(&scope.scope.get("$")), vec!["auto", "home"]);
    let response = slot_at(&ws, "dt", cell("r2", "in_enum"), "== \"");
    assert_eq!(labels(&response.slot.options), vec!["auto", "home"]);
}

#[test]
fn date_cell_keeps_date_with_output_column_writer() {
    let ws = writer_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r2", "in_date")));
    assert_eq!(scope.subject_type(), Some(VariableType::Date));
    assert_eq!(scope.scope.get("$"), VariableType::String);
    let response = slot_at(&ws, "dt", cell("r2", "in_date"), "> ");
    assert_eq!(response.slot.expected, Some(VariableType::Date));
    let response = slot_at(&ws, "dt", cell("r2", "in_date"), "$.");
    assert_eq!(response.slot.operand, Some(VariableType::String));
    let facts = ws.facts("policy");
    let fact = facts
        .iter()
        .find(|f| {
            f.block_id.as_ref() == "dt"
                && matches!(&f.target, CursorTarget::DecisionTableCell { row, col } if row.as_ref() == "r1" && col.as_ref() == "in_date")
        })
        .expect("date cell fact");
    assert_eq!(fact.subject_type, Some(VariableType::Date));
}

#[test]
fn undeclared_written_path_still_gets_written_type() {
    let ws = writer_workspace();
    let scope = scope_of(&ws, &cursor("dt", cell("r2", "in_total")));
    assert_eq!(scope.subject_type(), Some(VariableType::Number));
    let scope = scope_of(&ws, &cursor("dt", cell("r2", "out_band")));
    assert_eq!(
        enum_values(&scope.expected.expect("sibling union")),
        vec!["a", "b"]
    );
}

#[test]
fn empty_assertion_condition_resolves_for_completions_and_slot() {
    let ws = writer_workspace();
    let cursor = cursor_at("as_empty", expression("c2"), 0);
    let labels: Vec<String> = ws
        .completions(&cursor)
        .into_iter()
        .map(|c| c.label.to_string())
        .collect();
    assert!(labels.iter().any(|l| l == "claim"), "{labels:?}");
    let response = slot_at(&ws, "as_empty", expression("c2"), "");
    assert_eq!(response.slot.state, SlotState::Start);
    assert!(response.slot.auto_open);
}

#[test]
fn graph_table_heads_complete_for_outputs_and_fieldless_inputs() {
    let ws = graph_workspace();
    for col in ["o_dict", "c_condition"] {
        let labels: Vec<String> = ws
            .completions(&graph_cursor("dt", head(col)))
            .into_iter()
            .map(|c| c.label.to_string())
            .collect();
        assert!(labels.iter().any(|l| l == "age"), "{col}: {labels:?}");
    }
}

fn sibling_writer_workspace() -> PolicyWorkspace {
    import_scope_workspace(false)
}

fn import_scope_workspace(import_rule: bool) -> PolicyWorkspace {
    let shared = json!({
        "blocks": [
            {
                "id": "dict1",
                "type": "dictionary",
                "props": { "data": {
                    "name": "severity",
                    "entries": [
                        { "id": "e0", "value": "low", "label": "Low" },
                        { "id": "e1", "value": "high", "label": "High" }
                    ]
                } }
            },
            {
                "id": "dm",
                "type": "dataModel",
                "props": { "data": {
                    "name": "flight",
                    "properties": [
                        { "id": "p1", "name": "number", "type": "string", "array": false, "optional": false },
                        { "id": "p2", "name": "severity", "type": "relationship", "target": "severity", "array": false, "optional": false }
                    ]
                } },
                "children": []
            }
        ]
    });
    let rule = json!({
        "imports": ["shared"],
        "blocks": [
            {
                "id": "dt",
                "type": "decisionTable",
                "props": { "data": {
                    "hitPolicy": "first",
                    "inputs": [
                        { "id": "in_cond", "name": "Condition" }
                    ],
                    "outputs": [
                        { "id": "out_triggered", "name": "Triggered", "field": "rules.apu.triggered" }
                    ],
                    "rules": [
                        { "_id": "r1", "in_cond": "flight.number == \"X\"", "out_triggered": "true" },
                        { "_id": "r2", "in_cond": "", "out_triggered": "false" }
                    ]
                } },
                "children": []
            },
            { "id": "ex_a", "type": "expression", "props": { "data": { "key": "a", "value": "flight.number + \"!\"" } } },
            { "id": "ex_b", "type": "expression", "props": { "data": { "key": "b", "value": "len(a)" } } }
        ]
    });
    let entry = json!({
        "imports": if import_rule { vec!["shared", "rule"] } else { vec!["shared"] },
        "blocks": [
            { "id": "ex_triggered", "type": "expression", "props": { "data": { "key": "triggeredRules", "value": "rules.apu.triggered ? [\"APU\"] : []" } } },
            { "id": "ex_count", "type": "expression", "props": { "data": { "key": "triggeredCount", "value": "len(triggeredRules)" } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "shared",
        serde_json::from_value(shared).expect("valid shared fixture"),
    );
    ws.set_policy(
        "rule",
        serde_json::from_value(rule).expect("valid rule fixture"),
    );
    ws.set_policy(
        "entry",
        serde_json::from_value(entry).expect("valid entry fixture"),
    );
    ws
}

fn cursor_in(policy: &str, block: &str, target: CursorTarget) -> Cursor {
    Cursor {
        policy_path: policy.into(),
        block_id: block.into(),
        pos: 0,
        target,
    }
}

fn completion_labels(ws: &Workspace, cursor: &Cursor) -> Vec<String> {
    ws.completions(cursor)
        .into_iter()
        .map(|c| c.label.to_string())
        .collect()
}

#[test]
fn sibling_policy_writes_are_hidden_until_scheduled() {
    let ws = sibling_writer_workspace();

    let table_cell = cursor_in("rule", "dt", cell("r2", "in_cond"));
    let labels = completion_labels(&ws, &table_cell);
    assert!(labels.iter().any(|l| l == "flight"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "triggeredRules"), "{labels:?}");
    let scope = scope_of(&ws, &table_cell);
    assert_eq!(scope.scope.get("triggeredRules"), VariableType::Any);
    assert_eq!(
        scope.scope.get("flight").get("number"),
        VariableType::String
    );
    let mut probe = table_cell.clone();
    probe.pos = 15;
    let response = ws.slot(&probe, "triggeredRules ").expect("slot resolves");
    assert_eq!(response.slot.operand, Some(VariableType::Any));

    let own_write = cursor_in("rule", "ex_a", expression("ex_a"));
    let labels = completion_labels(&ws, &own_write);
    assert!(labels.iter().any(|l| l == "flight"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "a"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "triggeredRules"), "{labels:?}");

    let later_block = cursor_in("rule", "ex_b", expression("ex_b"));
    let labels = completion_labels(&ws, &later_block);
    assert!(labels.iter().any(|l| l == "a"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "triggeredRules"), "{labels:?}");
    let mut probe = later_block.clone();
    probe.pos = 2;
    let response = ws.slot(&probe, "a ").expect("slot resolves");
    assert_eq!(response.slot.operand, Some(VariableType::String));
    let scope = scope_of(&ws, &later_block);
    assert_eq!(scope.scope.get("a"), VariableType::String);
    assert_eq!(scope.scope.get("triggeredRules"), VariableType::Any);

    let facts = ws.facts("rule");
    let cell_fact = facts
        .iter()
        .find(|f| {
            f.block_id.as_ref() == "dt"
                && matches!(&f.target, CursorTarget::DecisionTableCell { row, col } if row.as_ref() == "r1" && col.as_ref() == "in_cond")
        })
        .expect("condition cell fact");
    assert_eq!(cell_fact.role, SlotRole::Condition);
    assert_eq!(cell_fact.expected_type, Some(VariableType::Bool));
    assert!(facts.iter().any(|f| f.block_id.as_ref() == "ex_b"));
}

#[test]
fn explicitly_imported_policy_writes_stay_visible_downstream() {
    let ws = import_scope_workspace(true);

    let reader = cursor_in("entry", "ex_triggered", expression("ex_triggered"));
    let labels = completion_labels(&ws, &reader);
    assert!(labels.iter().any(|l| l == "rules"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "flight"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "triggeredRules"), "{labels:?}");
    let scope = scope_of(&ws, &reader);
    assert_eq!(
        scope.scope.get("rules").get("apu").get("triggered"),
        VariableType::Bool
    );

    let after_write = cursor_in("entry", "ex_count", expression("ex_count"));
    let labels = completion_labels(&ws, &after_write);
    assert!(labels.iter().any(|l| l == "triggeredRules"), "{labels:?}");
    let mut probe = after_write.clone();
    probe.pos = 15;
    let response = ws.slot(&probe, "triggeredRules ").expect("slot resolves");
    assert_eq!(
        response.slot.operand,
        Some(VariableType::Array(
            VariableType::Const("APU".into()).into()
        ))
    );
}

#[test]
fn completions_agree_with_undefined_variable_diagnostics() {
    let mut ws = sibling_writer_workspace();
    let rule = json!({
        "imports": ["shared"],
        "blocks": [
            {
                "id": "dt",
                "type": "decisionTable",
                "props": { "data": {
                    "hitPolicy": "first",
                    "inputs": [
                        { "id": "in_cond", "name": "Condition" }
                    ],
                    "outputs": [
                        { "id": "out_triggered", "name": "Triggered", "field": "rules.apu.triggered" }
                    ],
                    "rules": [
                        { "_id": "r1", "in_cond": "len(triggeredRules) > 0", "out_triggered": "true" }
                    ]
                } },
                "children": []
            }
        ]
    });
    ws.set_policy(
        "rule",
        serde_json::from_value(rule).expect("valid rule fixture"),
    );

    let diagnostics = ws.diagnostics("rule");
    assert!(
        diagnostics
            .iter()
            .any(|d| d.location.block_id.as_deref() == Some("dt")
                && matches!(d.code, DiagnosticCode::UndefinedVariable)),
        "{diagnostics:?}"
    );
    let labels = completion_labels(&ws, &cursor_in("rule", "dt", cell("r1", "in_cond")));
    assert!(!labels.iter().any(|l| l == "triggeredRules"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "flight"), "{labels:?}");
}

#[test]
fn slot_expression_after_conditional_literal_union_offers_both_branches() {
    let doc = json!({
        "blocks": [
            { "id": "ex_g", "type": "expression", "props": { "data": { "key": "g", "value": "true ? \"hello\" : \"world\"" } } },
            { "id": "ex_h", "type": "expression", "props": { "data": { "key": "h", "value": "g == " } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("policy", serde_json::from_value(doc).unwrap());

    let scope = scope_of(&ws, &cursor("ex_h", expression("ex_h")));
    assert_eq!(
        scope.scope.get("g"),
        VariableType::Enum(None, vec!["hello".into(), "world".into()])
    );
    let response = slot_at(&ws, "ex_h", expression("ex_h"), "g == ");
    assert_eq!(response.slot.state, SlotState::Value);
    assert_eq!(labels(&response.slot.options), vec!["hello", "world"]);
}

#[test]
fn slot_scope_of_unparsable_block_keeps_earlier_independent_writes() {
    let doc = json!({
        "blocks": [
            { "id": "b1", "type": "expression", "props": { "data": { "key": "a", "value": "1" } } },
            { "id": "b2", "type": "expression", "props": { "data": { "key": "b", "value": "\"x\"" } } },
            { "id": "b3", "type": "expression", "props": { "data": { "key": "c", "value": "(" } } },
            { "id": "b4", "type": "expression", "props": { "data": { "key": "d", "value": "2" } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("policy", serde_json::from_value(doc).unwrap());

    let scope = scope_of(&ws, &cursor("b3", expression("b3")));
    assert_eq!(scope.scope.get("a"), VariableType::Number);
    assert_eq!(scope.scope.get("b"), VariableType::Const("x".into()));
    assert_eq!(scope.scope.get("d"), VariableType::Any);
}

#[test]
fn slot_and_completions_expose_closure_locals_in_expression_block() {
    let text = "map(m as x, map(x.tags as y, ";
    let doc = json!({
        "blocks": [
            { "id": "ex_m", "type": "expression", "props": { "data": { "key": "m", "value": "[{a: 10, tags: [\"x\"]}, {a: 20, tags: [\"y\"]}]" } } },
            { "id": "ex_n", "type": "expression", "props": { "data": { "key": "n", "value": text } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("policy", serde_json::from_value(doc).unwrap());

    let response = slot_at(&ws, "ex_n", expression("ex_n"), text);
    assert_eq!(response.slot.state, SlotState::Closure);
    let names: Vec<&str> = response
        .slot
        .locals
        .iter()
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(names, ["y", "x"]);
    assert_eq!(
        response.slot.locals[0].kind,
        VariableType::Enum(None, vec!["x".into(), "y".into()])
    );
    assert!(matches!(
        response.slot.locals[1].kind,
        VariableType::Object(_)
    ));

    let mut cursor = cursor("ex_n", expression("ex_n"));
    cursor.pos = text.len() as u32;
    let labels: Vec<String> = ws
        .completions(&cursor)
        .into_iter()
        .map(|c| c.label.to_string())
        .collect();
    assert_eq!(&labels[..3], ["y", "x", "m"]);

    let response = slot_at(&ws, "ex_n", expression("ex_n"), "map(m, #");
    assert_eq!(response.slot.locals.len(), 1);
    assert_eq!(response.slot.locals[0].name, "#");
    assert_eq!(response.slot.replace_span, (7, 8));
}

#[test]
fn value_slots_list_only_fields_that_can_fit_the_expected_scalar() {
    let ws = graph_workspace();
    let labels = |cursor: &Cursor| -> Vec<String> {
        ws.completions(cursor)
            .into_iter()
            .filter(|c| c.kind == CompletionKind::Variable)
            .map(|c| c.label.to_string())
            .collect()
    };
    // `> |` in a number cell: `age` fits, `name` cannot.
    let mut cursor = graph_cursor("dt", cell("r1", "c_field"));
    cursor.pos = 2;
    let after_gt = labels(&cursor);
    assert!(after_gt.iter().any(|l| l == "age"), "{after_gt:?}");
    assert!(!after_gt.iter().any(|l| l == "name"), "{after_gt:?}");
    // A condition start expects a bool but any field may open a comparison.
    let cursor = graph_cursor("dt", cell("r2", "c_condition"));
    let at_start = labels(&cursor);
    assert!(at_start.iter().any(|l| l == "name"), "{at_start:?}");
    // A schema-typed output cell filters like a comparison; a mixed one keeps everything.
    let typed = labels(&graph_cursor("dt", cell("r1", "o_schema")));
    assert!(!typed.iter().any(|l| l == "name"), "{typed:?}");
    let mixed = labels(&graph_cursor("dt_untyped", cell("u3", "o_mixed")));
    assert!(mixed.iter().any(|l| l == "name"), "{mixed:?}");
}

#[test]
fn operator_positions_offer_no_fields() {
    let ws = graph_workspace();
    // `age > 1|`: only an operator can follow, so the field list is empty.
    let mut cursor = graph_cursor("dt", cell("r1", "c_condition"));
    cursor.pos = 7;
    assert!(ws.completions(&cursor).is_empty());
    // `age >|`: the glued operator keeps the field list away too.
    cursor.pos = 5;
    assert!(ws.completions(&cursor).is_empty());
    // `age|`: the word itself still completes.
    cursor.pos = 3;
    assert!(ws.completions(&cursor).iter().any(|c| c.label == "age"));
}

#[test]
fn accepted_fields_know_what_follows_them() {
    let follow = |ws: &Workspace, cursor: &Cursor, label: &str| -> Option<String> {
        ws.completions(cursor)
            .into_iter()
            .find(|c| c.label == label)
            .and_then(|c| c.follow.map(String::from))
    };
    let ws = graph_workspace();
    // Condition cell at its start: an object continues with `.`, a leaf takes a space and an operator.
    let cursor = graph_cursor("dt", cell("r2", "c_condition"));
    assert_eq!(follow(&ws, &cursor, "age").as_deref(), Some(" "));
    assert_eq!(follow(&ws, &cursor, "$nodes").as_deref(), Some("."));
    // An output value never chains into a comparison.
    let cursor = graph_cursor("dt", cell("r1", "o_plain"));
    assert_eq!(follow(&ws, &cursor, "age"), None);
    // A policy unary cell chains; a table head (path) does not.
    let ws = policy_workspace();
    let at = cursor_at("dt", cell("r3", "in_condition"), 0);
    assert_eq!(follow(&ws, &at, "customer").as_deref(), Some("."));
    let at = cursor_at("dt", head("out_union"), 0);
    assert_eq!(follow(&ws, &at, "customer").as_deref(), Some("."));
}

#[test]
fn inspect_and_rename_use_utf16_after_non_ascii_literals() {
    let mut ws = PolicyWorkspace::new();
    let source = "\"😀é\" + customer.name";
    ws.set_policy("p", serde_json::from_value(json!({"blocks":[
        {"id":"dm", "type":"dataModel", "props":{"data":{"name":"customer", "properties":[
            {"id":"name", "name":"name", "type":"string", "optional":false, "array":false}
        ]}}},
        {"id":"ex_computed", "type":"expression", "props":{"data":{"key":"result", "value":source}}}
    ]})).unwrap());
    let mut at = cursor_in("p", "ex_computed", expression("ex_computed"));
    let start = source[..source.find("name").unwrap()]
        .encode_utf16()
        .count() as u32;
    at.pos = start + 2;
    let info = ws.inspect(&at).expect("member inspection");
    assert!(info.span.0 <= start && info.span.1 >= start + 4, "{info:?}");
    let rename = ws.prepare_rename(&at).expect("field rename");
    assert_eq!(rename.span, (start, start + 4));
    let edits = ws.rename(&rename.target, "fullName");
    assert!(
        edits.iter().any(|edit| match edit {
            zen_engine::policy::EngineEdit::ReplaceBlock {
                block_id,
                new_block,
                ..
            } if block_id.as_ref() == "ex_computed" => {
                new_block["props"]["data"]["value"] == "\"😀é\" + customer.fullName"
            }
            _ => false,
        }),
        "{edits:?}"
    );
}

#[test]
fn graph_collect_and_loop_expectations_preserve_array_valued_cells() {
    for hit_policy in ["first", "collect"] {
        for looped in [false, true] {
            for collected_column in [false, true] {
                let item = json!({"type":"string", "enum":["open","closed"]});
                let field = if collected_column {
                    json!({"type":"array", "items":item})
                } else {
                    item
                };
                let row = json!({"type":"object", "required":["tags"], "properties":{"tags":{
                    "type":"array", "items":field
                }}});
                let output = if hit_policy == "collect" {
                    json!({"type":"array", "items":row})
                } else {
                    row
                };
                let output = if looped {
                    json!({"type":"array", "items":output})
                } else {
                    output
                };
                let schema =
                    json!({"type":"object", "required":["result"], "properties":{"result":output}});
                let mut ws = Workspace::new();
                ws.set_document("g", document(json!({"nodes":[
                    node("in", "inputNode", json!({})),
                    node("calc", "decisionTableNode", json!({
                        "hitPolicy":hit_policy, "executionMode":if looped { "loop" } else { "single" },
                        "outputPath":"result", "passThrough":true, "inputs":[],
                        "outputs":[{"id":"o", "name":"Tags", "field":if collected_column { "tags[]" } else { "tags" }}],
                        "rules":[{"_id":"r", "o":""}]
                    })),
                    node("out", "outputNode", json!({"schema":schema.to_string()}))
                ], "edges":[edge("a","in","calc"), edge("b","calc","out")]})));
                let result = slot_at_graph(&ws, "calc", cell("r", "o"), "");
                assert!(
                    matches!(result.expected_type, Some(VariableType::Array(_))),
                    "{hit_policy}/{looped}/{collected_column}: {result:?}"
                );
                assert_eq!(result.slot.options[0].source.as_deref(), Some("[\"open\"]"));
            }
        }
    }
}

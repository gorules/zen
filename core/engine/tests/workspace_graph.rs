use std::sync::Arc;

use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{
    Cursor, CursorTarget, DiagnosticCode, EvaluateRequest, EvaluationError, ScopeRequest, Severity,
    Workspace,
};
use zen_expression::variable::VariableType;

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn person_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "age": { "type": "number" },
            "name": { "type": "string" }
        },
        "required": ["age", "name"]
    })
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn edge(id: &str, source: &str, target: &str) -> Value {
    json!({ "id": id, "sourceId": source, "targetId": target, "sourceHandle": null })
}

fn expression_node(id: &str, rows: &[(&str, &str)]) -> Value {
    let expressions: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({ "id": format!("{id}-e{i}"), "key": key, "value": value }))
        .collect();
    node(id, "expressionNode", json!({ "expressions": expressions }))
}

fn linear_graph(input_schema: Option<Value>, middle: Vec<Value>) -> Value {
    let mut nodes = vec![node(
        "in",
        "inputNode",
        match &input_schema {
            Some(schema) => json!({ "schema": schema.to_string() }),
            None => json!({}),
        },
    )];
    let mut edges = Vec::new();
    let mut prev = "in".to_string();
    for n in middle {
        let id = n["id"].as_str().unwrap().to_string();
        edges.push(edge(&format!("edge-{prev}-{id}"), &prev, &id));
        nodes.push(n);
        prev = id;
    }
    nodes.push(node("out", "outputNode", json!({})));
    edges.push(edge(&format!("edge-{prev}-out"), &prev, "out"));
    json!({ "nodes": nodes, "edges": edges })
}

fn error_codes(ws: &Workspace, path: &str) -> Vec<DiagnosticCode> {
    ws.diagnostics(path)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn valid_graph_has_no_diagnostics() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    assert!(ws.is_graph("g"));
    assert!(!ws.is_graph("missing"));
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn untyped_schema_positions_are_reported() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "legs": { "type": "array" },
            "meta": {},
            "age": { "type": "number" }
        },
        "required": ["legs", "age"]
    });
    ws.set_document(
        "g",
        document(linear_graph(
            Some(schema),
            vec![expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let implicit: Vec<&str> = diagnostics
        .iter()
        .filter(|d| matches!(d.code, DiagnosticCode::ImplicitAny))
        .map(|d| d.message.as_ref())
        .collect();
    assert_eq!(implicit.len(), 2, "{diagnostics:?}");
    assert!(
        implicit.iter().any(|m| m.contains("`legs[]`")),
        "{implicit:?}"
    );
    assert!(
        implicit.iter().any(|m| m.contains("`meta`")),
        "{implicit:?}"
    );

    ws.set_document(
        "typed",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    assert!(
        ws.diagnostics("typed")
            .iter()
            .all(|d| !matches!(d.code, DiagnosticCode::ImplicitAny)),
        "typed schema must not report ImplicitAny"
    );
}

#[test]
fn unknown_property_read_is_reported() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "salary * 2")])],
        )),
    );
    let codes = error_codes(&ws, "g");
    assert!(!codes.is_empty(), "expected an error for unknown property");
    let diagnostics = ws.diagnostics("g");
    let diag = diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
        .unwrap();
    assert_eq!(diag.location.block_id.as_deref(), Some("calc"));
    assert_eq!(diag.location.expression_id.as_deref(), Some("calc-e0"));
}

#[test]
fn expression_rows_see_previous_rows_via_dollar() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node(
                "calc",
                &[("total", "age * 2"), ("grand", "$.total + 1")],
            )],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn downstream_node_sees_only_upstream_output() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                expression_node("first", &[("total", "age * 2")]),
                expression_node("second", &[("doubled", "total * 2")]),
                expression_node("third", &[("bad", "age * 2")]),
            ],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].location.block_id.as_deref(), Some("third"));
}

#[test]
fn pass_through_preserves_input_downstream() {
    let mut ws = Workspace::new();
    let mut first = expression_node("first", &[("total", "age * 2")]);
    first["content"]["passThrough"] = json!(true);
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                first,
                expression_node("second", &[("bonus", "age + total")]),
            ],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn function_node_taints_downstream() {
    let mut ws = Workspace::new();
    let function = node(
        "fn",
        "functionNode",
        json!({ "source": "export const handler = (input) => input;" }),
    );
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                function,
                expression_node("after", &[("x", "whatever.deeply.unknown + 1")]),
            ],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(
        !diagnostics.iter().any(|d| d.severity == Severity::Error),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::UnresolvedFunctionType),
        "{diagnostics:?}"
    );

    let unchecked = ws.unchecked_nodes("g");
    let unchecked: Vec<&str> = unchecked.iter().map(|s| s.as_ref()).collect();
    assert!(unchecked.contains(&"fn"));
    assert!(unchecked.contains(&"after"));
    assert!(unchecked.contains(&"out"));
    assert!(!unchecked.contains(&"in"));
}

#[test]
fn parse_errors_still_reported_downstream_of_function() {
    let mut ws = Workspace::new();
    let function = node("fn", "functionNode", json!({ "source": "" }));
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![function, expression_node("after", &[("x", "1 +")])],
        )),
    );
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::ParseError),
        "expected parse error, got {codes:?}"
    );
}

#[test]
fn switch_condition_must_be_boolean() {
    let mut ws = Workspace::new();
    let switch = node(
        "sw",
        "switchNode",
        json!({
            "hitPolicy": "first",
            "statements": [
                { "id": "s1", "condition": "age" },
                { "id": "s2", "condition": "age > 10" },
                { "id": "s3", "condition": "" }
            ]
        }),
    );
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![switch])),
    );
    let diagnostics = ws.diagnostics("g");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, DiagnosticCode::TypeMismatch);
    assert_eq!(errors[0].location.expression_id.as_deref(), Some("s1"));
}

#[test]
fn decision_table_cells_are_checked() {
    let mut ws = Workspace::new();
    let table = node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [
                { "id": "c1", "name": "Age", "field": "age" }
            ],
            "outputs": [
                { "id": "o1", "name": "Result", "field": "result" }
            ],
            "rules": [
                { "_id": "r1", "c1": "> 18", "o1": "\"adult\"" },
                { "_id": "r2", "c1": "", "o1": "\"minor\"" }
            ]
        }),
    );
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![table])),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(
        diagnostics.iter().all(|d| d.severity == Severity::Hint),
        "{diagnostics:?}"
    );

    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let result = outputs.iter().find(|o| o.path.as_ref() == "result");
    assert!(result.is_some(), "{outputs:?}");
}

#[test]
fn decision_table_incompatible_output_cells_reported() {
    let mut ws = Workspace::new();
    let table = node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [
                { "id": "c1", "name": "Age", "field": "age" }
            ],
            "outputs": [
                { "id": "o1", "name": "Result", "field": "result" }
            ],
            "rules": [
                { "_id": "r1", "c1": "> 18", "o1": "\"adult\"" },
                { "_id": "r2", "c1": "<= 18", "o1": "{value: 1}" }
            ]
        }),
    );
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![table])),
    );
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::TypeMismatch),
        "expected type mismatch, got {codes:?}"
    );
}

#[test]
fn transform_loop_iterates_elements() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "customers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "age": { "type": "number" } },
                    "required": ["age"]
                }
            }
        },
        "required": ["customers"]
    });
    let mut looped = expression_node("loop", &[("doubled", "age * 2")]);
    looped["content"]["inputField"] = json!("customers");
    looped["content"]["executionMode"] = json!("loop");
    looped["content"]["outputPath"] = json!("results");
    ws.set_document("g", document(linear_graph(Some(schema), vec![looped])));
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let results = outputs
        .iter()
        .find(|o| o.path.as_ref() == "results")
        .expect("results output");
    assert!(
        matches!(results.resolved_type, VariableType::Array(_)),
        "{:?}",
        results.resolved_type
    );
}

#[test]
fn transform_loop_over_non_array_is_reported() {
    let mut ws = Workspace::new();
    let mut looped = expression_node("loop", &[("doubled", "age * 2")]);
    looped["content"]["inputField"] = json!("name");
    looped["content"]["executionMode"] = json!("loop");
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![looped])),
    );
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::TypeMismatch),
        "expected loop type mismatch, got {codes:?}"
    );
}

#[test]
fn decision_node_uses_referenced_policy_outputs() {
    let mut ws = Workspace::new();
    let policy = json!({
        "blocks": [
            {
                "id": "b1",
                "type": "expression",
                "props": { "data": { "key": "score", "value": "10" } }
            }
        ]
    });
    ws.set_document("scoring", document(policy));

    let decision = node("call", "decisionNode", json!({ "key": "scoring" }));
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                decision,
                expression_node("after", &[("final", "score + 1")]),
            ],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn missing_decision_reference_is_reported() {
    let mut ws = Workspace::new();
    let decision = node("call", "decisionNode", json!({ "key": "nowhere" }));
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![decision])),
    );
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::ImportNotFound),
        "expected import not found, got {codes:?}"
    );
}

#[test]
fn decision_node_between_graphs() {
    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("verdict", "age >= 18")])],
        )),
    );
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    ws.set_document(
        "parent",
        document(linear_graph(
            Some(person_schema()),
            vec![
                decision,
                expression_node("after", &[("ok", "verdict and true")]),
            ],
        )),
    );
    let diagnostics = ws.diagnostics("parent");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn cyclic_graph_is_reported() {
    let mut ws = Workspace::new();
    let content = json!({
        "nodes": [
            node("in", "inputNode", json!({})),
            expression_node("a", &[("x", "1")]),
            expression_node("b", &[("y", "2")]),
        ],
        "edges": [
            edge("e1", "in", "a"),
            edge("e2", "a", "b"),
            edge("e3", "b", "a"),
        ]
    });
    ws.set_document("g", document(content));
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::CyclicDependency),
        "{codes:?}"
    );
}

#[test]
fn edge_to_missing_node_is_reported() {
    let mut ws = Workspace::new();
    let content = json!({
        "nodes": [node("in", "inputNode", json!({}))],
        "edges": [edge("e1", "in", "ghost")]
    });
    ws.set_document("g", document(content));
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::InvalidGraphStructure),
        "{codes:?}"
    );
}

#[test]
fn missing_input_node_is_reported() {
    let mut ws = Workspace::new();
    let content = json!({
        "nodes": [expression_node("a", &[("x", "1")])],
        "edges": []
    });
    ws.set_document("g", document(content));
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::InvalidGraphStructure),
        "{codes:?}"
    );
}

#[test]
fn inputs_derive_from_schema() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    let inputs = ws.inputs(&ScopeRequest::for_policy("g"));
    let paths: Vec<&str> = inputs.iter().map(|i| i.path.as_ref()).collect();
    assert_eq!(paths, vec!["age", "name"]);
    assert!(matches!(inputs[0].resolved_type, VariableType::Number));
}

#[test]
fn inputs_inferred_from_reads_without_schema() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            None,
            vec![expression_node(
                "calc",
                &[("total", "customer.age * factor")],
            )],
        )),
    );
    let inputs = ws.inputs(&ScopeRequest::for_policy("g"));
    let paths: Vec<&str> = inputs.iter().map(|i| i.path.as_ref()).collect();
    assert_eq!(paths, vec!["customer.age", "factor"]);
}

#[test]
fn outputs_flow_through_output_node() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node(
                "calc",
                &[("total", "age * 2"), ("label", "name + '!'")],
            )],
        )),
    );
    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let mut paths: Vec<&str> = outputs.iter().map(|o| o.path.as_ref()).collect();
    paths.sort();
    assert_eq!(paths, vec!["label", "total"]);
    let total = outputs.iter().find(|o| o.path.as_ref() == "total").unwrap();
    assert!(matches!(total.resolved_type, VariableType::Number));
}

#[test]
fn inspect_and_completions_in_graph_expression() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    let cursor = Cursor {
        policy_path: "g".into(),
        block_id: "calc".into(),
        pos: 2,
        target: CursorTarget::Expression {
            id: "calc-e0".into(),
        },
    };
    let inspected = ws.inspect(&cursor).expect("inspect result");
    assert!(matches!(inspected.kind, VariableType::Number));

    let completions = ws.completions(&cursor);
    assert!(completions.iter().any(|c| c.label == "age"));
}

#[test]
fn graph_evaluation_is_rejected_by_workspace() {
    let mut ws = Workspace::new();
    ws.set_document("g", document(linear_graph(Some(person_schema()), vec![])));
    let result = ws.evaluate(&EvaluateRequest {
        policy_path: "g".into(),
        input: json!({}).into(),
        goals: vec![],
        trace: false,
    });
    assert!(matches!(result, Err(EvaluationError::GraphNotEvaluable(_))));
}

#[test]
fn mixed_workspace_diagnostics_cover_both_kinds() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "salary * 2")])],
        )),
    );
    let policy = json!({
        "blocks": [
            {
                "id": "b1",
                "type": "expression",
                "props": { "data": { "key": "score", "value": "unknownRoot + 1" } }
            }
        ]
    });
    ws.set_document("p", document(policy));

    let all = ws.all_diagnostics();
    let doc_paths: Vec<&str> = all
        .iter()
        .map(|d| d.location.policy_path.as_ref())
        .collect();
    assert!(doc_paths.contains(&"g"), "{all:?}");
    assert!(doc_paths.contains(&"p"), "{all:?}");
}

#[test]
fn output_schema_mismatch_reported() {
    let mut ws = Workspace::new();
    let out_schema = json!({
        "type": "object",
        "properties": {
            "total": { "type": "string" },
            "verdict": { "type": "boolean" }
        },
        "required": ["total", "verdict"]
    });
    let content = json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": person_schema().to_string() })),
            expression_node("calc", &[("total", "age * 2")]),
            node("out", "outputNode", json!({ "schema": out_schema.to_string() })),
        ],
        "edges": [edge("e1", "in", "calc"), edge("e2", "calc", "out")]
    });
    ws.set_document("g", document(content));
    let diagnostics = ws.diagnostics("g");
    let errors: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().any(|m| m.contains("'total'")), "{errors:?}");
    assert!(errors.iter().any(|m| m.contains("'verdict'")), "{errors:?}");
}

#[test]
fn output_schema_satisfied_is_quiet() {
    let mut ws = Workspace::new();
    let out_schema = json!({
        "type": "object",
        "properties": { "total": { "type": "number" } },
        "required": ["total"]
    });
    let content = json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": person_schema().to_string() })),
            expression_node("calc", &[("total", "age * 2")]),
            node("out", "outputNode", json!({ "schema": out_schema.to_string() })),
        ],
        "edges": [edge("e1", "in", "calc"), edge("e2", "calc", "out")]
    });
    ws.set_document("g", document(content));
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

fn simple_table(rules: Value) -> Value {
    node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [{ "id": "c1", "name": "Age", "field": "age" }],
            "outputs": [{ "id": "o1", "name": "Result", "field": "result" }],
            "rules": rules
        }),
    )
}

#[test]
fn uncovered_first_hit_table_is_nullable() {
    let mut ws = Workspace::new();
    let table = simple_table(json!([
        { "_id": "r1", "c1": "> 18", "o1": "\"adult\"" }
    ]));
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![table])),
    );
    let analysis = ws.graph_analysis("g").expect("analysis");
    assert!(
        matches!(analysis.signature.output, VariableType::Nullable(_)),
        "{:?}",
        analysis.signature.output
    );
}

#[test]
fn covered_first_hit_table_is_not_nullable() {
    let mut ws = Workspace::new();
    let table = simple_table(json!([
        { "_id": "r1", "c1": "> 18", "o1": "\"adult\"" },
        { "_id": "r2", "c1": "<= 18", "o1": "\"minor\"" }
    ]));
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![table])),
    );
    let analysis = ws.graph_analysis("g").expect("analysis");
    assert!(
        matches!(analysis.signature.output, VariableType::Object(_)),
        "{:?}",
        analysis.signature.output
    );
}

#[test]
fn catch_all_row_covers_table_and_empty_cell_makes_column_nullable() {
    let mut ws = Workspace::new();
    let table = simple_table(json!([
        { "_id": "r1", "c1": "> 18", "o1": "\"adult\"" },
        { "_id": "r2", "c1": "", "o1": "" }
    ]));
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![table])),
    );
    let analysis = ws.graph_analysis("g").expect("analysis");
    assert!(
        matches!(analysis.signature.output, VariableType::Object(_)),
        "{:?}",
        analysis.signature.output
    );
    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let result = outputs
        .iter()
        .find(|o| o.path.as_ref() == "result")
        .unwrap();
    assert!(
        matches!(result.resolved_type, VariableType::Nullable(_)),
        "{:?}",
        result.resolved_type
    );
}

#[test]
fn nl_projects_graph_expressions() {
    let mut ws = Workspace::new();
    let switch = node(
        "sw",
        "switchNode",
        json!({
            "hitPolicy": "first",
            "statements": [{ "id": "s1", "condition": "age > 18" }]
        }),
    );
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![switch, expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    let projected = ws.nl("g");
    let block_ids: Vec<&str> = projected.iter().map(|e| e.block_id.as_ref()).collect();
    assert!(block_ids.contains(&"sw"), "{block_ids:?}");
    assert!(block_ids.contains(&"calc"), "{block_ids:?}");
}

#[test]
fn unreachable_node_is_hinted() {
    let mut ws = Workspace::new();
    let content = json!({
        "nodes": [
            node("in", "inputNode", json!({})),
            expression_node("connected", &[("x", "1")]),
            expression_node("orphan", &[("y", "2")]),
        ],
        "edges": [edge("e1", "in", "connected")]
    });
    ws.set_document("g", document(content));
    let hints: Vec<_> = ws
        .diagnostics("g")
        .into_iter()
        .filter(|d| d.severity == Severity::Hint)
        .collect();
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert_eq!(hints[0].code, DiagnosticCode::UnreachableNode);
    assert_eq!(hints[0].location.block_id.as_deref(), Some("orphan"));
}

#[test]
fn redundant_parentheses_are_hinted() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "(age) * 2")])],
        )),
    );
    let hints: Vec<_> = ws
        .diagnostics("g")
        .into_iter()
        .filter(|d| d.code == DiagnosticCode::RedundantParentheses)
        .collect();
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert_eq!(hints[0].location.block_id.as_deref(), Some("calc"));
    assert_eq!(hints[0].location.expression_id.as_deref(), Some("calc-e0"));
}

#[test]
fn graph_property_rename_rewrites_writers_and_readers() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                expression_node("first", &[("total", "age * 2"), ("doubled", "$.total * 2")]),
                expression_node("second", &[("grand", "total + 1")]),
            ],
        )),
    );

    let cursor = Cursor {
        policy_path: "g".into(),
        block_id: "second".into(),
        pos: 2,
        target: CursorTarget::Expression {
            id: "second-e0".into(),
        },
    };
    let prepared = ws.prepare_rename(&cursor).expect("prepare rename");
    let RenameTarget::GraphProperty { document, path } = &prepared.target else {
        panic!("unexpected target {:?}", prepared.target);
    };
    assert_eq!(document.as_ref(), "g");
    assert_eq!(path.as_ref(), "total");
    assert_eq!(prepared.span, (0, 5));

    let sites = ws.references(&prepared.target);
    let kinds: Vec<String> = sites
        .iter()
        .map(|s| format!("{}:{:?}", s.block_id, s.kind))
        .collect();
    assert_eq!(sites.len(), 3, "{kinds:?}");

    let edits = ws.rename(&prepared.target, "sum");
    assert_eq!(edits.len(), 2, "{edits:?}");
    let rendered: Vec<String> = edits
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    let first_edit = rendered.iter().find(|r| r.contains("\"first\"")).unwrap();
    assert!(first_edit.contains("\"key\":\"sum\""), "{first_edit}");
    assert!(first_edit.contains("$.sum * 2"), "{first_edit}");
    let second_edit = rendered.iter().find(|r| r.contains("\"second\"")).unwrap();
    assert!(second_edit.contains("sum + 1"), "{second_edit}");
    for edit in &edits {
        assert!(matches!(edit, EngineEdit::ReplaceNode { .. }));
    }
}

#[test]
fn rename_of_unwritten_property_is_refused() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    let cursor = Cursor {
        policy_path: "g".into(),
        block_id: "calc".into(),
        pos: 2,
        target: CursorTarget::Expression {
            id: "calc-e0".into(),
        },
    };
    assert!(ws.prepare_rename(&cursor).is_none());
}

#[test]
fn graph_property_rename_through_output_path_and_input_field() {
    use zen_engine::policy::RenameTarget;

    let mut ws = Workspace::new();
    let mut producer = expression_node("producer", &[("total", "age * 2")]);
    producer["content"]["outputPath"] = json!("results");
    let mut consumer = expression_node("consumer", &[("grand", "total + 1")]);
    consumer["content"]["inputField"] = json!("results");
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![producer, consumer],
        )),
    );

    let target = RenameTarget::GraphProperty {
        document: "g".into(),
        path: "results.total".into(),
    };
    let sites = ws.references(&target);
    assert_eq!(sites.len(), 2, "{sites:?}");

    let edits = ws.rename(&target, "sum");
    let rendered: Vec<String> = edits
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    let producer_edit = rendered
        .iter()
        .find(|r| r.contains("\"producer\""))
        .unwrap();
    assert!(producer_edit.contains("\"key\":\"sum\""), "{producer_edit}");
    let consumer_edit = rendered
        .iter()
        .find(|r| r.contains("\"consumer\""))
        .unwrap();
    assert!(consumer_edit.contains("sum + 1"), "{consumer_edit}");
}

#[test]
fn nodes_scope_is_typed_from_ancestors() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                expression_node("first", &[("total", "age * 2")]),
                expression_node("second", &[("viaNodes", "$nodes.first.total")]),
            ],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let via = outputs
        .iter()
        .find(|o| o.path.as_ref() == "viaNodes")
        .expect("viaNodes output");
    assert!(
        matches!(via.resolved_type, VariableType::Number),
        "{:?}",
        via.resolved_type
    );

    let cursor = Cursor {
        policy_path: "g".into(),
        block_id: "second".into(),
        pos: 8,
        target: CursorTarget::Expression {
            id: "second-e0".into(),
        },
    };
    let completions = ws.completions(&Cursor {
        pos: 7,
        ..cursor.clone()
    });
    assert!(
        completions.iter().any(|c| c.label == "first"),
        "{completions:?}"
    );
}

#[test]
fn missing_input_schema_warns_and_marks_unchecked() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            None,
            vec![expression_node("calc", &[("total", "anything.at.all * 2")])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let warnings: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].code, DiagnosticCode::MissingInputSchema);
    assert_eq!(warnings[0].location.block_id.as_deref(), Some("in"));
    assert!(
        !diagnostics.iter().any(|d| d.severity == Severity::Error),
        "{diagnostics:?}"
    );

    let unchecked = ws.unchecked_nodes("g");
    let unchecked: Vec<&str> = unchecked.iter().map(|s| s.as_ref()).collect();
    assert!(unchecked.contains(&"in"), "{unchecked:?}");
    assert!(unchecked.contains(&"calc"), "{unchecked:?}");
    assert!(unchecked.contains(&"out"), "{unchecked:?}");
}

#[test]
fn decision_node_missing_required_input_is_reported() {
    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("verdict", "age >= 18")])],
        )),
    );
    let parent_schema = json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    });
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    ws.set_document(
        "parent",
        document(linear_graph(Some(parent_schema), vec![decision])),
    );
    let diagnostics = ws.diagnostics("parent");
    let errors: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("requires input 'age'"), "{errors:?}");
}

#[test]
fn decision_node_extra_input_is_allowed() {
    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("verdict", "age >= 18")])],
        )),
    );
    let parent_schema = json!({
        "type": "object",
        "properties": {
            "age": { "type": "number" },
            "name": { "type": "string" },
            "extra": { "type": "boolean" }
        },
        "required": ["age", "name", "extra"]
    });
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    ws.set_document(
        "parent",
        document(linear_graph(Some(parent_schema), vec![decision])),
    );
    let diagnostics = ws.diagnostics("parent");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn decision_node_incompatible_input_type_is_reported() {
    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("verdict", "age >= 18")])],
        )),
    );
    let parent_schema = json!({
        "type": "object",
        "properties": {
            "age": { "type": "string" },
            "name": { "type": "string" }
        },
        "required": ["age", "name"]
    });
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    ws.set_document(
        "parent",
        document(linear_graph(Some(parent_schema), vec![decision])),
    );
    let diagnostics = ws.diagnostics("parent");
    let errors: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("input 'age'") && errors[0].contains("`string`"),
        "{errors:?}"
    );
}

#[test]
fn graph_analysis_is_cached_across_unrelated_edits() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "age * 2")])],
        )),
    );
    let first = ws.graph_analysis("g").expect("analysis");

    ws.set_document(
        "unrelated",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("x", &[("y", "age")])],
        )),
    );
    let second = ws.graph_analysis("g").expect("analysis");
    assert!(
        Arc::ptr_eq(&first, &second),
        "unrelated edit must not invalidate"
    );

    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("total", "age * 3")])],
        )),
    );
    let third = ws.graph_analysis("g").expect("analysis");
    assert!(!Arc::ptr_eq(&second, &third), "own edit must invalidate");
}

#[test]
fn graph_analysis_invalidated_by_referenced_document_edit() {
    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("verdict", "age >= 18")])],
        )),
    );
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    ws.set_document(
        "parent",
        document(linear_graph(
            Some(person_schema()),
            vec![decision, expression_node("after", &[("ok", "verdict")])],
        )),
    );
    let first = ws.graph_analysis("parent").expect("analysis");
    let unchanged = ws.graph_analysis("parent").expect("analysis");
    assert!(Arc::ptr_eq(&first, &unchanged));

    ws.set_document(
        "child",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("verdict2", "age >= 21")])],
        )),
    );
    let after_child_edit = ws.graph_analysis("parent").expect("analysis");
    assert!(
        !Arc::ptr_eq(&first, &after_child_edit),
        "referenced doc edit must invalidate the caller"
    );
    let errors: Vec<_> = ws
        .diagnostics("parent")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
}

#[test]
fn cross_document_rename_from_callee_to_caller() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("calc", &[("verdict", "age >= 18")])],
        )),
    );
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    ws.set_document(
        "parent",
        document(linear_graph(
            Some(person_schema()),
            vec![
                decision,
                expression_node("after", &[("ok", "verdict and true")]),
            ],
        )),
    );

    let cursor = Cursor {
        policy_path: "parent".into(),
        block_id: "after".into(),
        pos: 2,
        target: CursorTarget::Expression {
            id: "after-e0".into(),
        },
    };
    let prepared = ws.prepare_rename(&cursor).expect("prepare rename");
    let RenameTarget::GraphProperty { document, path } = &prepared.target else {
        panic!("unexpected target {:?}", prepared.target);
    };
    assert_eq!(document.as_ref(), "child");
    assert_eq!(path.as_ref(), "verdict");

    let sites = ws.references(&prepared.target);
    let docs: Vec<&str> = sites.iter().map(|s| s.policy_path.as_ref()).collect();
    assert!(docs.contains(&"child"), "{docs:?}");
    assert!(docs.contains(&"parent"), "{docs:?}");

    let edits = ws.rename(&prepared.target, "isAdult");
    let mut edited: Vec<(&str, &str)> = edits
        .iter()
        .filter_map(|e| match e {
            EngineEdit::ReplaceNode {
                document, node_id, ..
            } => Some((document.as_ref(), node_id.as_ref())),
            _ => None,
        })
        .collect();
    edited.sort();
    assert_eq!(edited, vec![("child", "calc"), ("parent", "after")]);
    let rendered = serde_json::to_string(&edits).unwrap();
    assert!(rendered.contains("isAdult and true"), "{rendered}");
    assert!(rendered.contains("\"key\":\"isAdult\""), "{rendered}");
}

#[test]
fn policy_property_rename_reaches_calling_graphs() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    let policy = json!({
        "blocks": [
            {
                "id": "b1",
                "type": "expression",
                "props": { "data": { "key": "score", "value": "10" } }
            }
        ]
    });
    ws.set_document("scoring", document(policy));
    let decision = node("call", "decisionNode", json!({ "key": "scoring" }));
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                decision,
                expression_node("after", &[("final", "score + 1")]),
            ],
        )),
    );

    let target = RenameTarget::Global {
        name: "score".into(),
    };
    let sites = ws.references(&target);
    let docs: Vec<&str> = sites.iter().map(|s| s.policy_path.as_ref()).collect();
    assert!(docs.contains(&"scoring"), "{docs:?}");
    assert!(docs.contains(&"g"), "{docs:?}");

    let edits = ws.rename(&target, "points");
    let has_block_edit = edits
        .iter()
        .any(|e| matches!(e, EngineEdit::ReplaceBlock { .. }));
    let has_node_edit = edits
        .iter()
        .any(|e| matches!(e, EngineEdit::ReplaceNode { .. }));
    assert!(has_block_edit, "{edits:?}");
    assert!(has_node_edit, "{edits:?}");
    let rendered = serde_json::to_string(&edits).unwrap();
    assert!(rendered.contains("points + 1"), "{rendered}");
}

#[test]
fn cross_document_rename_from_graph_cursor_to_policy_target() {
    use zen_engine::policy::RenameTarget;

    let mut ws = Workspace::new();
    let policy = json!({
        "blocks": [
            {
                "id": "b1",
                "type": "expression",
                "props": { "data": { "key": "score", "value": "10" } }
            }
        ]
    });
    ws.set_document("scoring", document(policy));
    let decision = node("call", "decisionNode", json!({ "key": "scoring" }));
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![
                decision,
                expression_node("after", &[("final", "score + 1")]),
            ],
        )),
    );

    let cursor = Cursor {
        policy_path: "g".into(),
        block_id: "after".into(),
        pos: 2,
        target: CursorTarget::Expression {
            id: "after-e0".into(),
        },
    };
    let prepared = ws.prepare_rename(&cursor).expect("prepare rename");
    assert!(
        matches!(&prepared.target, RenameTarget::Global { name } if name.as_ref() == "score"),
        "{:?}",
        prepared.target
    );
}

fn typed_function_graph() -> Value {
    let function = node(
        "fn",
        "functionNode",
        json!({ "source": "export const handler = async (input: { age: number }) => ({ total: input.age * 2 });" }),
    );
    linear_graph(
        Some(person_schema()),
        vec![
            function,
            expression_node("after", &[("grand", "total + 1"), ("bad", "missing + 1")]),
        ],
    )
}

#[test]
fn resolver_types_function_nodes_and_downstream_is_checked() {
    let mut ws = Workspace::new();
    ws.set_function_resolver(|source, input| {
        assert!(source.contains("handler"));
        assert!(matches!(input, VariableType::Object(_)));
        Some("Promise<{ total: number }>".to_string())
    });
    ws.set_document("g", document(typed_function_graph()));

    let diagnostics = ws.diagnostics("g");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].location.block_id.as_deref(), Some("after"));
    assert!(errors[0].message.contains("missing"), "{errors:?}");
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::UnresolvedFunctionType),
        "{diagnostics:?}"
    );

    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let grand = outputs.iter().find(|o| o.path.as_ref() == "grand").unwrap();
    assert!(matches!(grand.resolved_type, VariableType::Number));
    assert!(ws.unchecked_nodes("g").is_empty());
}

#[test]
fn resolver_any_is_an_error() {
    let mut ws = Workspace::new();
    ws.set_function_resolver(|_, _| Some("any".to_string()));
    ws.set_document("g", document(typed_function_graph()));
    let diagnostics = ws.diagnostics("g");
    let function_errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error && d.location.block_id.as_deref() == Some("fn"))
        .collect();
    assert_eq!(function_errors.len(), 1, "{diagnostics:?}");
    assert!(
        function_errors[0].message.contains("`any`"),
        "{function_errors:?}"
    );
}

#[test]
fn resolver_null_keeps_node_opaque_with_warning() {
    let mut ws = Workspace::new();
    ws.set_function_resolver(|_, _| None);
    ws.set_document("g", document(typed_function_graph()));
    let diagnostics = ws.diagnostics("g");
    assert!(
        diagnostics.iter().any(|d| d.severity == Severity::Warning
            && d.code == DiagnosticCode::UnresolvedFunctionType
            && d.location.block_id.as_deref() == Some("fn")),
        "{diagnostics:?}"
    );
    let unchecked = ws.unchecked_nodes("g");
    assert!(!unchecked.is_empty());
}

#[test]
fn resolver_is_cached_per_source_and_input() {
    use std::cell::Cell;
    use std::rc::Rc;

    let calls = Rc::new(Cell::new(0));
    let seen = calls.clone();
    let mut ws = Workspace::new();
    ws.set_function_resolver(move |_, _| {
        seen.set(seen.get() + 1);
        Some("{ total: number }".to_string())
    });
    ws.set_document("g", document(typed_function_graph()));
    let _ = ws.diagnostics("g");
    let _ = ws.diagnostics("g");
    let _ = ws.outputs(&ScopeRequest::for_policy("g"));
    assert_eq!(calls.get(), 1);
}

#[test]
fn request_and_push_flow_without_native_resolver() {
    let mut ws = Workspace::new();
    ws.set_document("g", document(typed_function_graph()));

    let requests = ws.function_resolution_requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].source.contains("handler"));

    ws.set_function_type(
        &requests[0].source,
        &requests[0].input,
        Some("Promise<{ total: number }>"),
    );

    let diagnostics = ws.diagnostics("g");
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::UnresolvedFunctionType),
        "{diagnostics:?}"
    );
    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    assert!(outputs.iter().any(|o| o.path.as_ref() == "grand"));
    assert!(ws.function_resolution_requests().is_empty());
}

#[test]
fn resolver_sees_dynamic_input_types() {
    use std::cell::RefCell as StdRefCell;
    use std::rc::Rc;

    let seen_inputs = Rc::new(StdRefCell::new(Vec::new()));
    let sink = seen_inputs.clone();
    let mut ws = Workspace::new();
    ws.set_function_resolver(move |_, input| {
        sink.borrow_mut().push(format!("{input}"));
        Some("{ ok: boolean }".to_string())
    });
    let function = node(
        "fn",
        "functionNode",
        json!({ "source": "export const handler = (input) => ({ ok: true });" }),
    );
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![expression_node("pre", &[("total", "age * 2")]), function],
        )),
    );
    let _ = ws.diagnostics("g");
    let inputs = seen_inputs.borrow();
    assert_eq!(inputs.len(), 1, "{inputs:?}");
    assert_eq!(inputs[0], "object");
}

#[test]
fn strict_mode_flags_nested_and_closure_members() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "customer": {
                "type": "object",
                "properties": { "age": { "type": "number" } },
                "required": ["age"]
            },
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "qty": { "type": "number" } },
                    "required": ["qty"]
                }
            }
        },
        "required": ["customer", "items"]
    });
    ws.set_document(
        "g",
        document(linear_graph(
            Some(schema),
            vec![expression_node(
                "calc",
                &[
                    ("nested", "customer.salary"),
                    ("closure", "map(items, #.missing)"),
                    ("ok", "customer.age + sum(map(items, #.qty))"),
                ],
            )],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    let by_expr = |id: &str| {
        errors
            .iter()
            .filter(|d| d.location.expression_id.as_deref() == Some(id))
            .count()
    };
    assert_eq!(by_expr("calc-e0"), 1, "{errors:?}");
    assert_eq!(by_expr("calc-e1"), 1, "{errors:?}");
    assert_eq!(by_expr("calc-e2"), 0, "{errors:?}");
}

#[test]
fn strict_mode_requires_nullish_on_optional_properties() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "age": { "type": "number" },
            "bonus": { "type": "number" }
        },
        "required": ["age"]
    });
    ws.set_document(
        "g",
        document(linear_graph(
            Some(schema),
            vec![expression_node(
                "calc",
                &[("bad", "bonus + 1"), ("good", "(bonus ?? 0) + 1")],
            )],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].location.expression_id.as_deref(), Some("calc-e0"));
}

fn switch_graph(hit_policy: &str) -> Value {
    let schema = json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": ["shipping", "billing", "support"] },
            "amount": { "type": "number" }
        },
        "required": ["kind", "amount"]
    });
    json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": schema.to_string() })),
            node("sw", "switchNode", json!({
                "hitPolicy": hit_policy,
                "statements": [
                    { "id": "s1", "condition": "kind == 'shipping'" },
                    { "id": "s2", "condition": "kind == 'billing'" },
                    { "id": "s3", "condition": "" }
                ]
            })),
            expression_node("nShip", &[("k", "kind")]),
            expression_node("nBill", &[("k", "kind")]),
            expression_node("nRest", &[("k", "kind")]),
            node("out", "outputNode", json!({})),
        ],
        "edges": [
            edge("e0", "in", "sw"),
            { "id": "e1", "sourceId": "sw", "targetId": "nShip", "sourceHandle": "s1" },
            { "id": "e2", "sourceId": "sw", "targetId": "nBill", "sourceHandle": "s2" },
            { "id": "e3", "sourceId": "sw", "targetId": "nRest", "sourceHandle": "s3" },
            edge("e4", "nShip", "out"),
            edge("e5", "nBill", "out"),
            edge("e6", "nRest", "out"),
        ]
    })
}

fn node_input_kind(ws: &Workspace, path: &str, node_id: &str) -> VariableType {
    let analysis = ws.graph_analysis(path).expect("analysis");
    let node = analysis.nodes.get(node_id).expect("node");
    let VariableType::Object(fields) = &node.input else {
        panic!("{:?}", node.input);
    };
    let fields = fields.borrow();
    fields.get("kind").expect("kind").shallow_clone()
}

#[test]
fn switch_first_hit_narrows_branches_and_default() {
    let mut ws = Workspace::new();
    ws.set_document("g", document(switch_graph("first")));
    let diagnostics = ws.diagnostics("g");
    assert!(
        diagnostics.iter().all(|d| d.severity == Severity::Hint),
        "{diagnostics:?}"
    );

    assert!(
        matches!(node_input_kind(&ws, "g", "nShip"), VariableType::Const(ref c) if c.as_ref() == "shipping")
    );
    assert!(
        matches!(node_input_kind(&ws, "g", "nBill"), VariableType::Const(ref c) if c.as_ref() == "billing")
    );
    assert!(
        matches!(node_input_kind(&ws, "g", "nRest"), VariableType::Const(ref c) if c.as_ref() == "support")
    );
}

#[test]
fn switch_collect_narrows_positive_only() {
    let mut ws = Workspace::new();
    ws.set_document("g", document(switch_graph("collect")));

    assert!(
        matches!(node_input_kind(&ws, "g", "nBill"), VariableType::Const(ref c) if c.as_ref() == "billing")
    );
    let rest = node_input_kind(&ws, "g", "nRest");
    assert!(
        matches!(rest, VariableType::Enum(_, ref v) if v.len() == 3),
        "{rest:?}"
    );
}

#[test]
fn merged_branches_rewiden_after_join() {
    let mut ws = Workspace::new();
    ws.set_document("g", document(switch_graph("first")));
    let analysis = ws.graph_analysis("g").expect("analysis");
    let out = analysis.nodes.get("out").expect("out node");
    let VariableType::Object(fields) = &out.input else {
        panic!("{:?}", out.input);
    };
    let fields = fields.borrow();
    let joined = fields.get("k").expect("k");
    assert!(
        matches!(joined, VariableType::Enum(_, v) if v.len() == 3),
        "{joined:?}"
    );
}

fn typed_table(column_type: &str, cells: &[&str]) -> Value {
    let rules: Vec<Value> = cells
        .iter()
        .enumerate()
        .map(|(i, cell)| json!({ "_id": format!("r{i}"), "c1": "", "o1": cell }))
        .collect();
    node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [ { "id": "c1", "name": "Age", "field": "age" } ],
            "outputs": [ { "id": "o1", "name": "Score", "field": "score", "type": column_type } ],
            "rules": rules
        }),
    )
}

#[test]
fn graph_typed_output_column_rejects_mismatched_cells() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("number", &["10", "'high'"])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, DiagnosticCode::TypeMismatch);
    assert!(
        diagnostics[0]
            .message
            .contains("output cell must be `number`"),
        "{diagnostics:?}"
    );
    assert!(
        matches!(
            &diagnostics[0].location.target,
            Some(CursorTarget::DecisionTableCell { row, col })
                if row.as_ref() == "r1" && col.as_ref() == "o1"
        ),
        "{diagnostics:?}"
    );
}

#[test]
fn graph_typed_output_column_drives_schema() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("number", &["10", "20"])],
        )),
    );
    assert!(ws.diagnostics("g").is_empty(), "{:?}", ws.diagnostics("g"));
    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let score = outputs
        .iter()
        .find(|o| o.path.as_ref() == "score")
        .unwrap_or_else(|| panic!("{outputs:?}"));
    assert!(
        matches!(score.resolved_type, VariableType::Number),
        "{:?}",
        score.resolved_type
    );
}

#[test]
fn graph_typed_output_column_nullable_on_empty_cell() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("number", &["10", ""])],
        )),
    );
    let outputs = ws.outputs(&ScopeRequest::for_policy("g"));
    let score = outputs
        .iter()
        .find(|o| o.path.as_ref() == "score")
        .unwrap_or_else(|| panic!("{outputs:?}"));
    assert!(
        matches!(
            &score.resolved_type,
            VariableType::Nullable(inner) if matches!(inner.as_ref(), VariableType::Number)
        ),
        "{:?}",
        score.resolved_type
    );
}

#[test]
fn graph_typed_array_output_column() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("string[]", &["['a', 'b']", "[1]"])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        diagnostics[0]
            .message
            .contains("output cell must be `string[]`"),
        "{diagnostics:?}"
    );
}

#[test]
fn graph_unknown_output_type_diagnosed_on_head() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("customerTier", &["'VIP'"])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        diagnostics[0]
            .message
            .contains("unknown output type 'customerTier'"),
        "{diagnostics:?}"
    );
    assert!(
        matches!(
            &diagnostics[0].location.target,
            Some(CursorTarget::DecisionTableHead { col }) if col.as_ref() == "o1"
        ),
        "{diagnostics:?}"
    );
}

#[test]
fn graph_malformed_output_type_diagnosed() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("number[", &["10"])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        diagnostics[0].message.contains("invalid output type"),
        "{diagnostics:?}"
    );
}

#[test]
fn graph_nl_tokenize_output_cell_uses_declared_type() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("number", &["10"])],
        )),
    );
    let result = ws
        .nl_tokenize(
            &Cursor {
                policy_path: "g".into(),
                block_id: "dt".into(),
                pos: 0,
                target: CursorTarget::DecisionTableCell {
                    row: "r0".into(),
                    col: "o1".into(),
                },
            },
            "10",
        )
        .expect("tokenized");
    assert!(
        matches!(result.subject_type, Some(VariableType::Number)),
        "{:?}",
        result.subject_type
    );
}

#[test]
fn graph_untyped_output_column_keeps_inferred_behavior() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            Some(person_schema()),
            vec![typed_table("", &["10", "20"])],
        )),
    );
    assert!(ws.diagnostics("g").is_empty(), "{:?}", ws.diagnostics("g"));
    let results = ws.nl("g");
    let cell = results
        .iter()
        .find(|e| {
            matches!(
                &e.target,
                CursorTarget::DecisionTableCell { row, col }
                    if row.as_ref() == "r0" && col.as_ref() == "o1"
            )
        })
        .expect("cell projection");
    assert!(cell.result.subject_options.is_none());
}

#[test]
fn graph_dependencies_traverse_writers() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "cart": { "type": "object", "properties": { "total": { "type": "number" } }, "required": ["total"] }
        },
        "required": ["cart"]
    });
    let mut calc = expression_node("calc", &[("fee", "cart.total * 0.1")]);
    calc["content"]["passThrough"] = json!(true);
    let table = node(
        "tbl",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "outputPath": "price",
            "passThrough": true,
            "inputs": [
                { "id": "c1", "name": "Fee", "field": "fee" }
            ],
            "outputs": [
                { "id": "o1", "name": "Discount", "field": "discount" }
            ],
            "rules": [
                { "_id": "r1", "c1": "> 5", "o1": "0.2" },
                { "_id": "r2", "c1": "", "o1": "0.1" }
            ]
        }),
    );
    ws.set_document(
        "doc",
        document(linear_graph(Some(schema), vec![calc, table])),
    );

    let tree = ws.dependencies_scoped("price.discount", Some("doc"));
    assert_eq!(tree.property.as_ref(), "price.discount");
    let written_by = tree.written_by.as_ref().expect("table writes discount");
    assert_eq!(written_by.block_id.as_ref(), "tbl");
    assert_eq!(written_by.policy_path.as_ref(), "doc");

    let fee = tree
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "fee")
        .expect("discount depends on fee");
    assert_eq!(
        fee.written_by.as_ref().map(|w| w.block_id.as_ref()),
        Some("calc")
    );
    let total = fee
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "cart.total")
        .expect("fee depends on cart.total");
    assert!(total.written_by.is_none());
    assert!(total.deps.is_empty());
}

#[test]
fn graph_dependencies_resolve_array_element_types() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "price": { "type": "number" } },
                    "required": ["price"]
                }
            }
        },
        "required": ["items"]
    });
    let mut looped = expression_node("calc", &[("taxed", "price * 1.2")]);
    looped["content"]["inputField"] = json!("items");
    looped["content"]["executionMode"] = json!("loop");
    looped["content"]["outputPath"] = json!("results");
    ws.set_document("doc", document(linear_graph(Some(schema), vec![looped])));

    let tree = ws.dependencies_scoped("results", Some("doc"));
    let price = tree
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "items.price")
        .expect("results depend on items.price");
    assert!(
        matches!(price.resolved_type, VariableType::Number),
        "{:?}",
        price.resolved_type
    );
}

#[test]
fn graph_outputs_report_written_by() {
    let mut ws = Workspace::new();
    let mut calc = expression_node("calc", &[("fee", "age * 2")]);
    calc["content"]["passThrough"] = json!(true);
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![calc])),
    );

    let outputs = ws.outputs(&ScopeRequest::for_policy("doc"));
    let fee = outputs
        .iter()
        .find(|o| o.path.as_ref() == "fee")
        .expect("fee output present");
    assert_eq!(
        fee.written_by.as_ref().map(|w| w.block_id.as_ref()),
        Some("calc")
    );
    let age = outputs.iter().find(|o| o.path.as_ref() == "age");
    if let Some(age) = age {
        assert!(age.written_by.is_none(), "{age:?}");
    }
}

#[test]
fn graph_rename_rewrites_schema_declaration_and_reads() {
    use zen_engine::policy::{EngineEdit, ReferenceKind, RenameTarget};

    let mut ws = Workspace::new();
    let mut calc = expression_node("calc", &[("fee", "age * 2")]);
    calc["content"]["passThrough"] = json!(true);
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![calc])),
    );

    let target = RenameTarget::GraphProperty {
        document: Arc::from("doc"),
        path: Arc::from("age"),
    };

    let sites = ws.references(&target);
    assert!(
        sites
            .iter()
            .any(|s| s.kind == ReferenceKind::DataModel && s.block_id.as_ref() == "in"),
        "{sites:?}"
    );
    assert!(
        sites
            .iter()
            .any(|s| s.kind == ReferenceKind::ExpressionRead && s.block_id.as_ref() == "calc"),
        "{sites:?}"
    );

    let edits = ws.rename(&target, "years");
    let mut saw_schema = false;
    let mut saw_expression = false;
    for edit in &edits {
        let EngineEdit::ReplaceNode {
            node_id, new_node, ..
        } = edit
        else {
            continue;
        };
        match node_id.as_ref() {
            "in" => {
                let schema = &new_node["content"]["schema"];
                assert!(schema["properties"]["years"].is_object(), "{schema}");
                assert!(schema["properties"]["age"].is_null(), "{schema}");
                assert_eq!(schema["required"][0], json!("years"), "{schema}");
                saw_schema = true;
            }
            "calc" => {
                let value = new_node["content"]["expressions"][0]["value"]
                    .as_str()
                    .unwrap();
                assert_eq!(value, "years * 2");
                saw_expression = true;
            }
            _ => {}
        }
    }
    assert!(saw_schema, "{edits:?}");
    assert!(saw_expression, "{edits:?}");
}

#[test]
fn graph_node_rename_rewrites_nodes_references() {
    use zen_engine::policy::{EngineEdit, ReferenceKind, RenameTarget};

    let mut ws = Workspace::new();
    let mut score = expression_node("score", &[("score", "age * 2")]);
    score["name"] = json!("Score");
    score["content"]["passThrough"] = json!(true);
    let mut merge = expression_node(
        "merge",
        &[
            ("viaDot", "$nodes.Score.score + 1"),
            ("viaBracket", "$nodes['Score'].score + 2"),
        ],
    );
    merge["content"]["passThrough"] = json!(true);
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![score, merge])),
    );

    let target = RenameTarget::GraphNode {
        document: Arc::from("doc"),
        node_id: Arc::from("score"),
    };

    let sites = ws.references(&target);
    assert_eq!(
        sites
            .iter()
            .filter(|s| s.kind == ReferenceKind::ExpressionRead)
            .count(),
        2,
        "{sites:?}"
    );

    let edits = ws.rename(&target, "Scorer");
    let mut renamed_node = false;
    let mut rewrote_refs = false;
    for edit in &edits {
        let EngineEdit::ReplaceNode {
            node_id, new_node, ..
        } = edit
        else {
            continue;
        };
        match node_id.as_ref() {
            "score" => {
                assert_eq!(new_node["name"], json!("Scorer"));
                renamed_node = true;
            }
            "merge" => {
                let rows = new_node["content"]["expressions"].as_array().unwrap();
                assert_eq!(rows[0]["value"], json!("$nodes.Scorer.score + 1"));
                assert_eq!(rows[1]["value"], json!("$nodes['Scorer'].score + 2"));
                rewrote_refs = true;
            }
            _ => {}
        }
    }
    assert!(renamed_node, "{edits:?}");
    assert!(rewrote_refs, "{edits:?}");
}

#[test]
fn graph_property_rename_rewrites_function_sources() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    let function = node(
        "fn",
        "functionNode",
        json!({ "source": "export const handler = async (input) => {\n  return { score: input.age * 2 };\n};\n" }),
    );
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![function])),
    );

    let target = RenameTarget::GraphProperty {
        document: Arc::from("doc"),
        path: Arc::from("age"),
    };

    let sites = ws.references(&target);
    let fn_site = sites
        .iter()
        .find(|s| s.block_id.as_ref() == "fn")
        .expect("function read site present");
    assert!(!fn_site.source.contains('\n'), "{fn_site:?}");
    assert!(fn_site.source.contains("input.age"), "{fn_site:?}");

    let edits = ws.rename(&target, "years");
    let fn_edit = edits
        .iter()
        .find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                node_id, new_node, ..
            } if node_id.as_ref() == "fn" => Some(new_node),
            _ => None,
        })
        .expect("function node rewritten");
    let source = fn_edit["content"]["source"].as_str().unwrap();
    assert!(source.contains("input.years * 2"), "{source}");
    assert!(!source.contains("input.age"), "{source}");
}

#[test]
fn graph_dependencies_survive_cyclic_calls() {
    let mut ws = Workspace::new();
    let call_b = node("call-b", "decisionNode", json!({ "key": "b" }));
    let call_a = node("call-a", "decisionNode", json!({ "key": "a" }));
    ws.set_document("a", document(linear_graph(None, vec![call_b])));
    ws.set_document("b", document(linear_graph(None, vec![call_a])));

    let tree = ws.dependencies_scoped("x", Some("a"));
    assert_eq!(tree.property.as_ref(), "x");
    assert!(tree.written_by.is_none());
    let _ = ws.outputs(&ScopeRequest::for_policy("a"));

    let mut ws = Workspace::new();
    let self_call = node("call", "decisionNode", json!({ "key": "loop" }));
    ws.set_document("loop", document(linear_graph(None, vec![self_call])));
    let tree = ws.dependencies_scoped("x", Some("loop"));
    assert!(tree.written_by.is_none());
}

#[test]
fn graph_opaque_output_path_not_reported_as_writer() {
    let mut ws = Workspace::new();
    let mut calc = expression_node("calc", &[("total", "1 + 1")]);
    calc["content"]["outputPath"] = json!("price-list");
    ws.set_document("doc", document(linear_graph(None, vec![calc])));

    let tree = ws.dependencies_scoped("total", Some("doc"));
    assert!(tree.written_by.is_none(), "{tree:?}");
}

#[test]
fn graph_dependencies_diamond_keeps_written_by() {
    let mut ws = Workspace::new();
    let mut wc = expression_node("wc", &[("c", "1")]);
    wc["content"]["passThrough"] = json!(true);
    let mut wab = expression_node("wab", &[("a", "c + 1"), ("b", "c + 2")]);
    wab["content"]["passThrough"] = json!(true);
    let mut wx = expression_node("wx", &[("x", "a + b")]);
    wx["content"]["passThrough"] = json!(true);
    ws.set_document("doc", document(linear_graph(None, vec![wc, wab, wx])));

    let tree = ws.dependencies_scoped("x", Some("doc"));
    let a = tree
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "a")
        .expect("x depends on a");
    let b = tree
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "b")
        .expect("x depends on b");
    let c_under_a = a
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "c")
        .expect("a depends on c");
    let c_under_b = b
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "c")
        .expect("b depends on c");
    assert_eq!(
        c_under_a.written_by.as_ref().map(|w| w.block_id.as_ref()),
        Some("wc")
    );
    assert_eq!(
        c_under_b.written_by.as_ref().map(|w| w.block_id.as_ref()),
        Some("wc"),
        "second diamond occurrence must keep written_by"
    );
}

#[test]
fn graph_property_rename_preserves_node_identity() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    let mut calc = expression_node("total", &[("total", "1 + 1")]);
    calc["name"] = json!("total");
    calc["content"]["passThrough"] = json!(true);
    ws.set_document("doc", document(linear_graph(None, vec![calc])));

    let target = RenameTarget::GraphProperty {
        document: Arc::from("doc"),
        path: Arc::from("total"),
    };
    let edits = ws.rename(&target, "sum");
    let new_node = edits
        .iter()
        .find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                node_id, new_node, ..
            } if node_id.as_ref() == "total" => Some(new_node),
            _ => None,
        })
        .expect("node rewritten");
    assert_eq!(new_node["id"], json!("total"), "{new_node}");
    assert_eq!(new_node["name"], json!("total"), "{new_node}");
    assert_eq!(new_node["content"]["expressions"][0]["key"], json!("sum"));
}

#[test]
fn graph_node_rename_preserves_equal_strings_and_handles_empty_name() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    let table = node(
        "tbl",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "passThrough": true,
            "inputs": [],
            "outputs": [{ "id": "o1", "name": "Total", "field": "Total" }],
            "rules": [{ "_id": "r1", "o1": "1" }]
        }),
    );
    let mut table = table;
    table["name"] = json!("Total");
    ws.set_document("doc", document(linear_graph(None, vec![table])));

    let target = RenameTarget::GraphNode {
        document: Arc::from("doc"),
        node_id: Arc::from("tbl"),
    };
    let edits = ws.rename(&target, "Score");
    let new_node = edits
        .iter()
        .find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                node_id, new_node, ..
            } if node_id.as_ref() == "tbl" => Some(new_node),
            _ => None,
        })
        .expect("node renamed");
    assert_eq!(new_node["name"], json!("Score"), "{new_node}");
    assert_eq!(new_node["content"]["outputs"][0]["field"], json!("Total"));
    assert_eq!(new_node["content"]["outputs"][0]["name"], json!("Total"));

    let mut ws = Workspace::new();
    let mut anon = expression_node("anon", &[("", "")]);
    anon["name"] = json!("");
    ws.set_document("doc", document(linear_graph(None, vec![anon])));
    let target = RenameTarget::GraphNode {
        document: Arc::from("doc"),
        node_id: Arc::from("anon"),
    };
    let edits = ws.rename(&target, "Named");
    let new_node = edits
        .iter()
        .find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                node_id, new_node, ..
            } if node_id.as_ref() == "anon" => Some(new_node),
            _ => None,
        })
        .expect("empty-named node renamed");
    assert_eq!(new_node["name"], json!("Named"), "{new_node}");
    assert_eq!(new_node["content"]["expressions"][0]["key"], json!(""));
    assert_eq!(new_node["content"]["expressions"][0]["value"], json!(""));
}

#[test]
fn graph_property_rename_skips_function_strings_and_comments() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    let source = "export const handler = async (input) => {\n  // input.age is doubled\n  const msg = \"input.age missing\";\n  return { score: input.age * 2, msg };\n};\n";
    let function = node("fn", "functionNode", json!({ "source": source }));
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![function])),
    );

    let target = RenameTarget::GraphProperty {
        document: Arc::from("doc"),
        path: Arc::from("age"),
    };
    let edits = ws.rename(&target, "years");
    let fn_edit = edits
        .iter()
        .find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                node_id, new_node, ..
            } if node_id.as_ref() == "fn" => Some(new_node),
            _ => None,
        })
        .expect("function node rewritten");
    let rewritten = fn_edit["content"]["source"].as_str().unwrap();
    assert!(rewritten.contains("input.years * 2"), "{rewritten}");
    assert!(rewritten.contains("// input.age is doubled"), "{rewritten}");
    assert!(rewritten.contains("\"input.age missing\""), "{rewritten}");
}

#[test]
fn graph_rename_handles_multibyte_sources() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    let mut calc = expression_node("calc", &[("flagged", "name == 'é' and age > 2")]);
    calc["content"]["passThrough"] = json!(true);
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![calc])),
    );

    let target = RenameTarget::GraphProperty {
        document: Arc::from("doc"),
        path: Arc::from("age"),
    };
    let edits = ws.rename(&target, "years");
    let calc_edit = edits
        .iter()
        .find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                node_id, new_node, ..
            } if node_id.as_ref() == "calc" => Some(new_node),
            _ => None,
        })
        .expect("expression rewritten");
    assert_eq!(
        calc_edit["content"]["expressions"][0]["value"],
        json!("name == 'é' and years > 2")
    );
}

#[test]
fn graph_schema_rename_guards_conflicts_and_descends_items() {
    use zen_engine::policy::{EngineEdit, RenameTarget};

    let mut ws = Workspace::new();
    let mut calc = expression_node("calc", &[("first", "age * 2")]);
    calc["content"]["passThrough"] = json!(true);
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![calc])),
    );
    let target = RenameTarget::GraphProperty {
        document: Arc::from("doc"),
        path: Arc::from("age"),
    };
    let edits = ws.rename(&target, "name");
    let schema_edit = edits.iter().find_map(|edit| match edit {
        EngineEdit::ReplaceNode {
            node_id, new_node, ..
        } if node_id.as_ref() == "in" => Some(new_node),
        _ => None,
    });
    assert!(
        schema_edit.is_none(),
        "conflicting rename must not touch schema: {edits:?}"
    );

    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "orders": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "price": { "type": "number" } },
                    "required": ["price"]
                }
            }
        },
        "required": ["orders"]
    });
    let mut calc = expression_node("calc", &[("total", "sum(map(orders, #.price))")]);
    calc["content"]["passThrough"] = json!(true);
    ws.set_document("doc", document(linear_graph(Some(schema), vec![calc])));
    let target = RenameTarget::GraphProperty {
        document: Arc::from("doc"),
        path: Arc::from("orders.price"),
    };
    let edits = ws.rename(&target, "amount");
    let schema_edit = edits
        .iter()
        .find_map(|edit| match edit {
            EngineEdit::ReplaceNode {
                node_id, new_node, ..
            } if node_id.as_ref() == "in" => Some(new_node),
            _ => None,
        })
        .expect("schema rewritten for array items: {edits:?}");
    let schema = &schema_edit["content"]["schema"];
    assert!(
        schema["properties"]["orders"]["items"]["properties"]["amount"].is_object(),
        "{schema}"
    );
    assert!(
        schema["properties"]["orders"]["items"]["properties"]["price"].is_null(),
        "{schema}"
    );
    assert_eq!(
        schema["properties"]["orders"]["items"]["required"][0],
        json!("amount"),
        "{schema}"
    );
}

#[test]
fn graph_collect_table_output_type_flows_downstream() {
    let mut ws = Workspace::new();
    let table = node(
        "tbl",
        "decisionTableNode",
        json!({
            "hitPolicy": "collect",
            "inputs": [{ "id": "c1", "name": "Age", "field": "age" }],
            "outputs": [{ "id": "o1", "name": "Result", "field": "result" }],
            "rules": [{ "_id": "r1", "c1": "> 1", "o1": "1" }]
        }),
    );
    let post = expression_node("post", &[("x", "result + 1")]);
    ws.set_document(
        "doc",
        document(linear_graph(Some(person_schema()), vec![table, post])),
    );

    let codes = error_codes(&ws, "doc");
    assert!(
        !codes.is_empty(),
        "reading a property off the collect array must be flagged"
    );
}

#[test]
fn graph_pass_through_uncovered_table_fields_are_optional() {
    let make = |expr: &str| {
        let mut ws = Workspace::new();
        let table = node(
            "tbl",
            "decisionTableNode",
            json!({
                "hitPolicy": "first",
                "passThrough": true,
                "inputs": [{ "id": "c1", "name": "Age", "field": "age" }],
                "outputs": [{ "id": "o1", "name": "Result", "field": "result" }],
                "rules": [{ "_id": "r1", "c1": "> 100", "o1": "1" }]
            }),
        );
        let mut post = expression_node("post", &[("x", expr)]);
        post["content"]["passThrough"] = json!(true);
        ws.set_document(
            "doc",
            document(linear_graph(Some(person_schema()), vec![table, post])),
        );
        ws
    };

    let guarded = make("(result ?? 0) + 1");
    let codes = error_codes(&guarded, "doc");
    assert!(codes.is_empty(), "guarded read must be clean: {codes:?}");
    assert!(
        guarded
            .diagnostics("doc")
            .iter()
            .all(|d| !matches!(d.code, DiagnosticCode::RedundantNullish)),
        "?? on an uncovered table field is meaningful, not redundant"
    );

    let bare = make("result + 1");
    assert!(
        error_codes(&bare, "doc").contains(&DiagnosticCode::TypeMismatch),
        "bare arithmetic on a possibly-missing table field must be flagged"
    );

    let outputs = guarded.outputs(&ScopeRequest::for_policy("doc"));
    assert!(
        outputs.iter().any(|o| o.path.as_ref() == "result"),
        "written field stays a derived output"
    );
    assert!(
        outputs
            .iter()
            .all(|o| o.path.as_ref() != "age" && o.path.as_ref() != "name"),
        "untouched pass-through inputs must not appear as outputs: {:?}",
        outputs.iter().map(|o| o.path.as_ref()).collect::<Vec<_>>()
    );
}

#[test]
fn graph_typed_column_null_cells_are_nullable() {
    let make_ws = |cells: [&str; 2], column_type: Option<&str>| {
        let mut ws = Workspace::new();
        let mut output_col = json!({ "id": "o1", "name": "Score", "field": "score" });
        if let Some(ct) = column_type {
            output_col["type"] = json!(ct);
        }
        let table = node(
            "tbl",
            "decisionTableNode",
            json!({
                "hitPolicy": "first",
                "passThrough": true,
                "inputs": [{ "id": "c1", "name": "Age", "field": "age" }],
                "outputs": [output_col],
                "rules": [
                    { "_id": "r1", "c1": "> 10", "o1": cells[0] },
                    { "_id": "r2", "c1": "", "o1": cells[1] }
                ]
            }),
        );
        let post = expression_node("post", &[("x", "score + 1")]);
        ws.set_document(
            "doc",
            document(linear_graph(Some(person_schema()), vec![table, post])),
        );
        ws
    };

    let untyped = error_codes(&make_ws(["10", "null"], None), "doc");
    let typed = error_codes(&make_ws(["10", "null"], Some("number")), "doc");
    assert_eq!(
        typed, untyped,
        "typed column with null cells must behave like untyped"
    );
}

#[test]
fn graph_signature_excludes_unreachable_sinks() {
    let mut ws = Workspace::new();
    let mut calc = expression_node("calc", &[("fee", "age * 2")]);
    calc["content"]["passThrough"] = json!(true);
    let mut graph = linear_graph(Some(person_schema()), vec![calc]);
    let mut junk = expression_node("junk", &[("debug", "1")]);
    junk["content"]["passThrough"] = json!(true);
    graph["nodes"].as_array_mut().unwrap().push(junk);
    ws.set_document("doc", document(graph));

    let outputs = ws.outputs(&ScopeRequest::for_policy("doc"));
    assert!(
        !outputs.iter().any(|o| o.path.as_ref() == "debug"),
        "unreachable sink must not leak into the signature: {outputs:?}"
    );
    assert!(
        outputs.iter().any(|o| o.path.as_ref() == "fee"),
        "{outputs:?}"
    );
}

fn rates_loop_graph(hit_policy: &str, rules: Value, pass_through: bool) -> Value {
    let schema = json!({
        "type": "object",
        "properties": {
            "rates": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "region": { "type": "string" }, "amount": { "type": "number" } },
                    "required": ["region", "amount"]
                }
            }
        },
        "required": ["rates"]
    });
    let table = node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": hit_policy,
            "passThrough": pass_through,
            "inputField": "rates",
            "executionMode": "loop",
            "outputPath": "results",
            "inputs": [{ "id": "c1", "name": "Amount", "field": "amount" }],
            "outputs": [{ "id": "o1", "name": "Rate", "field": "rate" }],
            "rules": rules
        }),
    );
    let reader = node(
        "read",
        "expressionNode",
        json!({
            "passThrough": true,
            "expressions": [
                { "id": "e1", "key": "phantom", "value": "map(results, #.inclusive ?? false)" },
                { "id": "e2", "key": "real", "value": "map(results, #.rate ?? 0)" },
                { "id": "e3", "key": "carried", "value": "map(results, #.amount)" }
            ]
        }),
    );
    json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": schema.to_string() })),
            table,
            reader,
            node("out", "outputNode", json!({})),
        ],
        "edges": [
            edge("g1", "in", "dt"),
            edge("g2", "dt", "read"),
            edge("g3", "read", "out"),
        ]
    })
}

#[test]
fn loop_table_output_columns_propagate_into_element_type() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(rates_loop_graph(
            "first",
            json!([
                { "_id": "r1", "c1": "> 100", "o1": "0.12" },
                { "_id": "r2", "c1": "", "o1": "0.02" }
            ]),
            true,
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let phantom: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::UndefinedVariable)
        .collect();
    assert_eq!(phantom.len(), 1, "{diagnostics:?}");
    assert!(
        phantom[0].message.contains("inclusive"),
        "the never-produced field must be the one flagged: {diagnostics:?}"
    );
}

#[test]
fn loop_collect_table_elements_are_row_arrays() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(rates_loop_graph(
            "collect",
            json!([{ "_id": "r1", "c1": "> 100", "o1": "0.12" }]),
            true,
        )),
    );
    let analysis = ws.graph_analysis("g").expect("analysis");
    let dt = analysis.nodes.get("dt").expect("dt node");
    let results = dt.output.get("results");
    let element = match &results {
        VariableType::Array(inner) => inner.as_ref().shallow_clone(),
        other => panic!("results must be an array, got {other:?}"),
    };
    assert!(
        matches!(element, VariableType::Array(_)),
        "collect in loop must produce row arrays per element, got {element:?}"
    );
    let member_errors = analysis
        .diagnostics
        .iter()
        .filter(|d| d.location.block_id.as_deref() == Some("read") && d.severity == Severity::Error)
        .count();
    assert!(
        member_errors >= 3,
        "member reads on row arrays must be flagged: {:?}",
        analysis.diagnostics
    );
}

#[test]
fn pass_through_nullable_patch_merges_fields_as_optional() {
    let mut ws = Workspace::new();
    let child_table = simple_table(json!([
        { "_id": "r1", "c1": "> 18", "o1": "\"adult\"" }
    ]));
    let mut child = linear_graph(Some(person_schema()), vec![child_table]);
    child["nodes"].as_array_mut().unwrap()[1]["content"]["passThrough"] = json!(false);
    ws.set_document("child", document(child));

    let decision = node(
        "call",
        "decisionNode",
        json!({ "key": "child", "passThrough": true }),
    );
    ws.set_document(
        "parent",
        document(linear_graph(Some(person_schema()), vec![decision])),
    );

    let outputs = ws.outputs(&ScopeRequest::for_policy("parent"));
    let result = outputs
        .iter()
        .find(|o| o.path.as_ref() == "result")
        .expect("result output");
    assert!(
        matches!(result.resolved_type, VariableType::Nullable(_)),
        "a nullable pass-through patch must merge its fields as optional, got {:?}",
        result.resolved_type
    );
}

fn line_items_child(required_item_fields: Value) -> Value {
    let schema = json!({
        "type": "object",
        "properties": {
            "lineItems": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "amount": { "type": "number" },
                        "inclusive": { "type": "boolean" }
                    },
                    "required": required_item_fields
                }
            }
        },
        "required": ["lineItems"]
    });
    linear_graph(
        Some(schema),
        vec![expression_node(
            "calc",
            &[("total", "sum(map(lineItems, #.amount))")],
        )],
    )
}

fn line_items_parent() -> Value {
    let schema = json!({
        "type": "object",
        "properties": {
            "source": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "amount": { "type": "number" } },
                    "required": ["amount"]
                }
            }
        },
        "required": ["source"]
    });
    let mut mk = expression_node("mk", &[("lineItems", "map(source, { amount: #.amount })")]);
    mk["content"]["passThrough"] = json!(true);
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    linear_graph(Some(schema), vec![mk, decision])
}

#[test]
fn decision_boundary_reports_item_level_diff() {
    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(line_items_child(json!(["amount", "inclusive"]))),
    );
    ws.set_document("parent", document(line_items_parent()));
    let diagnostics = ws.diagnostics("parent");
    let errors: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("lineItems[].inclusive") && errors[0].contains("`bool`"),
        "the diff must name the item field, not flatten to object[]: {errors:?}"
    );
}

#[test]
fn decision_boundary_allows_missing_optional_item_field() {
    let mut ws = Workspace::new();
    ws.set_document("child", document(line_items_child(json!(["amount"]))));
    ws.set_document("parent", document(line_items_parent()));
    let diagnostics = ws.diagnostics("parent");
    assert!(
        diagnostics.is_empty(),
        "an optional item field the parent never produces must not break the boundary: {diagnostics:?}"
    );
}

#[test]
fn optional_property_without_null_type_warns_of_divergence() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "age": { "type": "number" },
            "name": { "type": "string" },
            "alias": { "type": ["string", "null"] },
            "tags": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "label": { "type": "string" }, "weight": { "type": "number" } },
                    "required": ["label"]
                }
            }
        },
        "required": ["age", "tags"]
    });
    ws.set_document(
        "g",
        document(linear_graph(
            Some(schema),
            vec![expression_node("calc", &[("x", "age * 2")])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let divergent: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::NullabilityDivergence)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(divergent.len(), 2, "{divergent:?}");
    assert!(
        divergent.iter().any(|m| m.contains("`name`")),
        "{divergent:?}"
    );
    assert!(
        divergent.iter().any(|m| m.contains("`tags[].weight`")),
        "{divergent:?}"
    );
    assert!(
        !divergent.iter().any(|m| m.contains("`alias`")),
        "a type that allows null must not warn: {divergent:?}"
    );
}

#[test]
fn decision_boundary_names_nullability_delta() {
    let mut ws = Workspace::new();
    ws.set_document(
        "child",
        document(line_items_child(json!(["amount", "inclusive"]))),
    );
    let parent_schema = json!({
        "type": "object",
        "properties": {
            "lineItems": {
                "type": ["array", "null"],
                "items": {
                    "type": "object",
                    "properties": {
                        "amount": { "type": "number" },
                        "inclusive": { "type": "boolean" }
                    },
                    "required": ["amount", "inclusive"]
                }
            }
        },
        "required": ["lineItems"]
    });
    let decision = node("call", "decisionNode", json!({ "key": "child" }));
    ws.set_document(
        "parent",
        document(linear_graph(Some(parent_schema), vec![decision])),
    );
    let diagnostics = ws.diagnostics("parent");
    let errors: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("lineItems") && errors[0].contains("may be null"),
        "the nullability delta must be stated, not flattened: {errors:?}"
    );
}

#[test]
fn any_typed_graph_output_is_an_error_in_strict_graphs() {
    let mut ws = Workspace::new();
    let graph = json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": person_schema().to_string() })),
            expression_node("a", &[("x", "$nodes.b.marker")]),
            expression_node("b", &[("marker", "1")]),
        ],
        "edges": [edge("e1", "in", "a"), edge("e2", "in", "b")]
    });
    ws.set_document("g", document(graph));
    let diagnostics = ws.diagnostics("g");
    let implicit: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::ImplicitAny && d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(implicit.len(), 1, "{diagnostics:?}");
    assert!(
        implicit[0].contains("`x`"),
        "the any-typed output path must be named: {implicit:?}"
    );
}

#[test]
fn recursive_decision_any_output_is_an_error() {
    let mut ws = Workspace::new();
    let decision = node("call", "decisionNode", json!({ "key": "g" }));
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![decision])),
    );
    let diagnostics = ws.diagnostics("g");
    let implicit: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::ImplicitAny && d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert!(
        implicit.iter().any(|m| m.contains("resolves to `any`")),
        "a recursive sub-decision degrades the result to any and must error: {diagnostics:?}"
    );
}

#[test]
fn schemaless_graph_output_any_stays_warning_only() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(linear_graph(
            None,
            vec![expression_node("calc", &[("double", "value * 2")])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(
        diagnostics.iter().all(|d| d.severity != Severity::Error),
        "without an input schema the graph stays warning-only: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::MissingInputSchema),
        "{diagnostics:?}"
    );
}

#[test]
fn string_literal_columns_hint_dictionary_candidates() {
    let mut ws = Workspace::new();
    let table = node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [{ "id": "c1", "name": "Name", "field": "name" }],
            "outputs": [{ "id": "o1", "name": "Verdict", "field": "verdict" }],
            "rules": [
                { "_id": "r1", "c1": "\"gold\"", "o1": "\"approve\"" },
                { "_id": "r2", "c1": "\"silver\"", "o1": "\"review\"" },
                { "_id": "r3", "c1": "", "o1": "\"reject\"" }
            ]
        }),
    );
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![table])),
    );
    let diagnostics = ws.diagnostics("g");
    let hints: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::PreferDictionary)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(hints.len(), 2, "{diagnostics:?}");
    assert!(
        hints
            .iter()
            .any(|m| m.contains("'name'") && m.contains("\"gold\" | \"silver\"")),
        "{hints:?}"
    );
    assert!(
        hints
            .iter()
            .any(|m| m.contains("'verdict'") && m.contains("\"approve\" | \"review\" | \"reject\"")),
        "{hints:?}"
    );
}

#[test]
fn inline_schema_enum_hints_dictionary() {
    let mut ws = Workspace::new();
    let schema = json!({
        "type": "object",
        "properties": {
            "status": { "type": "string", "enum": ["active", "suspended"] },
            "tier": { "$dictionary": "customerTier" }
        },
        "required": ["status"]
    });
    ws.set_document(
        "g",
        document(linear_graph(
            Some(schema),
            vec![expression_node("calc", &[("s", "status")])],
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let hints: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.code == DiagnosticCode::PreferDictionary)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(hints.len(), 1, "{diagnostics:?}");
    assert!(hints[0].contains("`status`"), "{hints:?}");
}

#[test]
fn date_literal_cells_do_not_hint_dictionary() {
    let mut ws = Workspace::new();
    let table = node(
        "dt",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [{ "id": "c1", "name": "Name", "field": "name" }],
            "outputs": [{ "id": "o1", "name": "Rate", "field": "rate" }],
            "rules": [
                { "_id": "r1", "c1": "\"2024-01-01\"", "o1": "0.1" },
                { "_id": "r2", "c1": "\"2018-01-01\"", "o1": "0.2" }
            ]
        }),
    );
    ws.set_document(
        "g",
        document(linear_graph(Some(person_schema()), vec![table])),
    );
    let diagnostics = ws.diagnostics("g");
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::PreferDictionary),
        "date-keyed columns are calendars, not enums: {diagnostics:?}"
    );
}

#[test]
fn schemaless_graph_suppresses_type_derived_expression_errors() {
    let mut ws = Workspace::new();
    let graph = json!({
        "nodes": [
            node("in", "inputNode", json!({})),
            expression_node("first", &[("count", "len(items)")]),
            expression_node("second", &[("label", "$nodes.first.count + \"!\"")]),
        ],
        "edges": [edge("e1", "in", "first"), edge("e2", "first", "second")]
    });
    ws.set_document("g", document(graph));
    let diagnostics = ws.diagnostics("g");
    assert!(
        diagnostics.iter().all(|d| d.severity != Severity::Error),
        "an unchecked graph must not raise type-derived errors: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == DiagnosticCode::MissingInputSchema),
        "{diagnostics:?}"
    );
}

#[test]
fn strict_graph_still_reports_nodes_scope_type_errors() {
    let mut ws = Workspace::new();
    let graph = json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": person_schema().to_string() })),
            expression_node("first", &[("count", "age * 2")]),
            expression_node("second", &[("label", "$nodes.first.count + \"!\"")]),
        ],
        "edges": [edge("e1", "in", "first"), edge("e2", "first", "second")]
    });
    ws.set_document("g", document(graph));
    let codes = error_codes(&ws, "g");
    assert!(
        codes.contains(&DiagnosticCode::TypeMismatch),
        "strict graphs keep type checking: {codes:?}"
    );
}

fn grouped_items_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "items": { "type": "array", "items": {
                "type": "object",
                "properties": {
                    "grp": { "type": ["string", "null"] },
                    "lines": { "type": ["array", "null"], "items": {
                        "type": "object", "properties": { "idx": { "type": ["number", "null"] } } } }
                }
            } }
        },
        "required": ["items"]
    })
}

fn single_expression_graph(schema: Value, expr: &str) -> Value {
    json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": schema.to_string() })),
            node("ex", "expressionNode", json!({ "expressions": [{ "id": "e1", "key": "out", "value": expr }] })),
        ],
        "edges": [edge("e1", "in", "ex")]
    })
}

#[test]
fn assignment_bound_locals_keep_element_types_in_closures() {
    let cases = [
        r#"map(items as m, (gp = filter(items as x, x.grp == m.grp)[0]; mw = filter(gp.lines ?? [] as c, c.idx == 0)[0]; mw))"#,
        r#"(loc = filter(items as x, x.grp == "a"); map(loc as e, e.grp))"#,
        r#"map(items as m, (gp = filter(items as x, x.grp == m.grp); len(gp) + (gp[0].grp == "a" ? 1 : 0)))"#,
    ];
    for expr in cases {
        let mut ws = Workspace::new();
        ws.set_document(
            "g",
            document(single_expression_graph(grouped_items_schema(), expr)),
        );
        let codes = error_codes(&ws, "g");
        assert!(
            codes.is_empty(),
            "closure over a ;-bound local must type-check: {expr}\n{codes:?}"
        );
    }
}

#[test]
fn missing_member_through_local_names_the_member_not_the_alias() {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        document(single_expression_graph(
            grouped_items_schema(),
            r#"(loc = filter(items as x, x.grp == "a"); map(loc as e, e.doesNotExist))"#,
        )),
    );
    let diagnostics = ws.diagnostics("g");
    let errors: Vec<&str> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "{diagnostics:?}");
    assert!(
        errors[0].contains("doesNotExist") && !errors[0].contains("'e'"),
        "the specific member must be blamed, not the alias: {errors:?}"
    );
}

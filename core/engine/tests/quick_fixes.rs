use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{Diagnostic, DiagnosticCode, PolicyWorkspace, Workspace};

fn policy_expression(value: &str) -> Vec<Diagnostic> {
    let doc = json!({ "blocks": [
        { "id": "dm", "type": "dataModel", "props": { "data": {
            "name": "applicant",
            "properties": [
                { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false },
                { "id": "p2", "name": "vip", "type": "boolean", "array": false, "optional": false }
            ]
        } } },
        { "id": "calc", "type": "expression", "props": { "data": { "key": "applicant.total", "value": value } } }
    ] });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).expect("policy"));
    ws.diagnostics("p")
}

fn graph_expression(value: &str) -> Vec<Diagnostic> {
    let schema = json!({
        "type": "object",
        "properties": { "age": { "type": "number" }, "vip": { "type": "boolean" } },
        "required": ["age", "vip"]
    });
    let content: DecisionContent = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": { "schema": schema.to_string() } },
            { "id": "calc", "name": "calc", "type": "expressionNode", "content": {
                "expressions": [ { "id": "x", "key": "total", "value": value } ],
                "passThrough": true
            } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "calc" },
            { "id": "e2", "sourceId": "calc", "targetId": "out" }
        ]
    }))
    .expect("graph");
    let mut ws = Workspace::new();
    ws.set_document("g", content);
    ws.diagnostics("g")
}

fn with_code(diagnostics: &[Diagnostic], code: DiagnosticCode) -> Vec<Diagnostic> {
    diagnostics
        .iter()
        .filter(|d| d.code == code)
        .cloned()
        .collect()
}

fn arg(diagnostic: &Diagnostic, key: &str) -> Option<String> {
    diagnostic.args.get(key).cloned()
}

#[test]
fn redundant_parentheses_offer_verified_fixes() {
    for (run, prefix) in [
        (
            policy_expression as fn(&str) -> Vec<Diagnostic>,
            "applicant.",
        ),
        (graph_expression, ""),
    ] {
        let source = format!("(({prefix}age)) + ({prefix}age * 2)");
        let found = with_code(&run(&source), DiagnosticCode::RedundantParentheses);
        assert!(found.len() >= 2, "{prefix}: {found:?}");
        for d in &found {
            assert_eq!(arg(d, "fixOriginal").as_deref(), Some(source.as_str()));
            let fixed = arg(d, "fixSource").expect("fix");
            assert!(fixed.len() < source.len(), "{fixed}");
        }
        let all = arg(&found[0], "fixAll").expect("fix all");
        assert_eq!(all, format!("{prefix}age + {prefix}age * 2"));
        assert!(with_code(&run(&all), DiagnosticCode::RedundantParentheses).is_empty());
    }
}

#[test]
fn removing_parentheses_keeps_words_apart() {
    let source = "applicant.age > 1 and(applicant.vip)";
    let found = with_code(
        &policy_expression(source),
        DiagnosticCode::RedundantParentheses,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        arg(&found[0], "fixSource").as_deref(),
        Some("applicant.age > 1 and applicant.vip")
    );
}

#[test]
fn redundant_fallbacks_are_dropped() {
    let found = with_code(
        &graph_expression("(age ?? 0) * 2"),
        DiagnosticCode::RedundantNullish,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    let d = &found[0];
    assert_eq!(arg(d, "fixSource").as_deref(), Some("age * 2"));
    assert_eq!(arg(d, "fixKeep").as_deref(), Some("left"));
    assert_eq!(arg(d, "fixFallback").as_deref(), Some("0"));
    assert!(with_code(
        &graph_expression("age * 2"),
        DiagnosticCode::RedundantNullish
    )
    .is_empty());

    let found = with_code(
        &graph_expression("age + (vip ?? false ? 1 : 0)"),
        DiagnosticCode::RedundantNullish,
    );
    for d in &found {
        let fixed = arg(d, "fixSource").expect("fix");
        assert!(
            with_code(&graph_expression(&fixed), DiagnosticCode::RedundantNullish).len()
                < found.len()
        );
    }
}

#[test]
fn empty_columns_name_the_column() {
    let doc: Value = json!({ "blocks": [
        { "id": "dm", "type": "dataModel", "props": { "data": {
            "name": "applicant",
            "properties": [ { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false } ]
        } } },
        { "id": "dt", "type": "decisionTable", "props": { "data": {
            "hitPolicy": "first",
            "inputs": [
                { "id": "i0", "name": "Age", "field": "applicant.age" },
                { "id": "i1", "name": "Unused", "field": "applicant.age" }
            ],
            "outputs": [ { "id": "o0", "name": "Band", "field": "applicant.band" } ],
            "rules": [
                { "_id": "r1", "i0": "< 18", "i1": "", "o0": "'minor'" },
                { "_id": "r2", "i0": ">= 18", "i1": "", "o0": "'adult'" }
            ]
        } } }
    ] });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).expect("policy"));
    let found = with_code(
        &ws.diagnostics("p"),
        DiagnosticCode::NonDiscriminatingColumn,
    );
    assert!(
        found
            .iter()
            .any(|d| arg(d, "emptyColumn").as_deref() == Some("i1")),
        "{found:?}"
    );
}

fn stress_graph(total: &str, condition: &str) -> Value {
    let schema = json!({
        "type": "object",
        "properties": {
            "age": { "type": "number" },
            "vip": { "type": "boolean" },
            "items": { "type": "array", "items": {
                "type": "object",
                "properties": { "kind": { "type": "string" }, "amount": { "type": "number" } },
                "required": ["kind", "amount"]
            } }
        },
        "required": ["age", "vip", "items"]
    });
    json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": { "schema": schema.to_string() } },
            { "id": "calc", "name": "calc", "type": "expressionNode", "content": {
                "expressions": [ { "id": "x", "key": "total", "value": total } ],
                "passThrough": true
            } },
            { "id": "dt", "name": "dt", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "inputs": [ { "id": "c0", "name": "Cond" } ],
                "outputs": [ { "id": "o0", "name": "Hit", "field": "hit" } ],
                "rules": [
                    { "_id": "r1", "c0": condition, "o0": "true" },
                    { "_id": "r2", "c0": "", "o0": "false" }
                ],
                "passThrough": true
            } },
            { "id": "sw", "name": "sw", "type": "switchNode", "content": {
                "hitPolicy": "first",
                "statements": [ { "id": "s1", "condition": condition }, { "id": "s2", "condition": "" } ]
            } },
            { "id": "a", "name": "a", "type": "expressionNode", "content": {
                "expressions": [ { "id": "ax", "key": "branch", "value": "'a'" } ], "passThrough": true
            } },
            { "id": "b", "name": "b", "type": "expressionNode", "content": {
                "expressions": [ { "id": "bx", "key": "branch", "value": "'b'" } ], "passThrough": true
            } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "calc" },
            { "id": "e2", "sourceId": "calc", "targetId": "dt" },
            { "id": "e3", "sourceId": "dt", "targetId": "sw" },
            { "id": "e4", "sourceId": "sw", "targetId": "a", "sourceHandle": "s1" },
            { "id": "e5", "sourceId": "sw", "targetId": "b", "sourceHandle": "s2" },
            { "id": "e6", "sourceId": "a", "targetId": "out" },
            { "id": "e7", "sourceId": "b", "targetId": "out" }
        ]
    })
}

fn replace_strings(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(s) if s == from => *s = to.to_string(),
        Value::Array(items) => items.iter_mut().for_each(|v| replace_strings(v, from, to)),
        Value::Object(map) => map.values_mut().for_each(|v| replace_strings(v, from, to)),
        _ => {}
    }
}

fn graph_diagnostics(graph: &Value) -> Vec<Diagnostic> {
    let mut ws = Workspace::new();
    ws.set_document("g", serde_json::from_value(graph.clone()).expect("graph"));
    ws.diagnostics("g")
}

#[tokio::test]
async fn complex_expressions_converge_and_keep_results() {
    converge_and_compare(false).await;
}

#[tokio::test]
async fn complex_expressions_converge_with_remove_all() {
    converge_and_compare(true).await;
}

async fn converge_and_compare(prefer_all: bool) {
    let total = "sum(map(filter(flatten([(items ?? []), ((items ?? []))]), (#.amount ?? 0) > (1)), (#.amount ?? 0))) + ((age ?? 0) * (2))";
    let condition =
        "((vip ?? false)) and (some((items ?? []), (#.kind == 'x'))) or ((age ?? 0) > (30))";
    let original = stress_graph(total, condition);
    let fixable = |d: &Diagnostic| {
        matches!(
            d.code,
            DiagnosticCode::RedundantParentheses | DiagnosticCode::RedundantNullish
        )
    };
    let before: Vec<Diagnostic> = graph_diagnostics(&original)
        .into_iter()
        .filter(fixable)
        .collect();
    assert!(before.len() >= 10, "{}", before.len());

    let mut graph = original.clone();
    let mut applied = 0;
    for _ in 0..200 {
        let next = graph_diagnostics(&graph)
            .into_iter()
            .filter(fixable)
            .find_map(|d| {
                let all = prefer_all.then(|| arg(&d, "fixAll")).flatten();
                Some((
                    arg(&d, "fixOriginal")?,
                    all.or_else(|| arg(&d, "fixSource"))?,
                ))
            });
        let Some((from, to)) = next else {
            break;
        };
        replace_strings(&mut graph, &from, &to);
        applied += 1;
    }
    let left: Vec<String> = graph_diagnostics(&graph)
        .into_iter()
        .filter(fixable)
        .map(|d| d.message)
        .collect();
    assert!(left.is_empty(), "unfixed: {left:?}");
    assert!(applied >= if prefer_all { 2 } else { 10 }, "{applied}");

    let evaluate = |graph: Value| async move {
        let DecisionContent::Graph(content) = serde_json::from_value(graph).expect("graph") else {
            panic!("graph");
        };
        let decision = zen_engine::Decision::from(content);
        let mut results = Vec::new();
        for age in [0, 25, 31, 70] {
            for vip in [true, false] {
                for items in [
                    json!([]),
                    json!([{ "kind": "x", "amount": 5 }, { "kind": "y", "amount": 1 }]),
                    json!([{ "kind": "y", "amount": 3 }]),
                ] {
                    let input = json!({ "age": age, "vip": vip, "items": items });
                    let response = decision.evaluate(input.into()).await.expect("evaluate");
                    let output: Value = response.result.into();
                    results.push(output);
                }
            }
        }
        results
    };
    let expected = evaluate(original).await;
    let actual = evaluate(graph.clone()).await;
    assert_eq!(expected, actual, "{graph}");
}

#[test]
fn redundant_fallbacks_fix_all_at_once() {
    for (run, prefix) in [
        (
            policy_expression as fn(&str) -> Vec<Diagnostic>,
            "applicant.",
        ),
        (graph_expression, ""),
    ] {
        let source = format!("({prefix}age ?? 0) * 2 + ({prefix}age ?? 1)");
        let found = with_code(&run(&source), DiagnosticCode::RedundantNullish);
        assert_eq!(found.len(), 2, "{prefix}: {found:?}");
        let all = arg(&found[0], "fixAll").expect("fix all");
        assert_eq!(arg(&found[1], "fixAll").as_deref(), Some(all.as_str()));
        assert_eq!(all, format!("{prefix}age * 2 + {prefix}age"));
        assert!(with_code(&run(&all), DiagnosticCode::RedundantNullish).is_empty());
    }
}

use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zen_engine::loader::MemoryLoader;
use zen_engine::model::{DecisionContent, PolicyContent};
use zen_engine::policy::{Diagnostic, DiagnosticCode, PolicyWorkspace, Workspace};
use zen_engine::DecisionEngine;

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

fn applicant_model() -> Value {
    json!({ "id": "dm", "type": "dataModel", "props": { "data": {
        "name": "applicant",
        "properties": [
            { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false },
            { "id": "p2", "name": "target", "type": "number", "array": false, "optional": true },
            { "id": "p3", "name": "vip", "type": "boolean", "array": false, "optional": false }
        ]
    } } })
}

fn policy_diagnostics(blocks: Vec<Value>) -> Vec<Diagnostic> {
    let mut all = vec![applicant_model()];
    all.extend(blocks);
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(json!({ "blocks": all })).expect("policy"),
    );
    ws.diagnostics("p")
}

fn policy_expression(value: &str) -> Vec<Diagnostic> {
    policy_diagnostics(vec![
        json!({ "id": "calc", "type": "expression", "props": { "data": { "key": "applicant.total", "value": value } } }),
    ])
}

fn graph_expression(value: &str) -> Vec<Diagnostic> {
    let schema = json!({
        "type": "object",
        "properties": { "amount": { "type": "number" }, "target": { "type": "number" } },
        "required": ["amount"]
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

fn cell_table(field: &str, cell: &str) -> Value {
    json!({ "id": "dt", "type": "decisionTable", "props": { "data": {
        "hitPolicy": "first",
        "inputs": [ { "id": "i0", "name": "In", "field": field } ],
        "outputs": [ { "id": "o0", "name": "Rate", "field": "applicant.rate" } ],
        "rules": [
            { "_id": "r1", "i0": cell, "o0": "1" },
            { "_id": "r2", "i0": "", "o0": "2" }
        ]
    } } })
}

#[test]
fn unary_fallback_fix_keeps_boolean_semantics() {
    for cell in [
        "applicant.vip ?? false",
        "$ ?? false",
        "(applicant.vip ?? true)",
    ] {
        let found = with_code(
            &policy_diagnostics(vec![cell_table("applicant.vip", cell)]),
            DiagnosticCode::RedundantNullish,
        );
        assert_eq!(found.len(), 1, "{cell}: {found:?}");
        assert_eq!(arg(&found[0], "fixSource"), None, "{cell}: {found:?}");
    }

    let found = with_code(
        &policy_diagnostics(vec![cell_table(
            "applicant.age",
            "(applicant.age ?? 0) + 1",
        )]),
        DiagnosticCode::RedundantNullish,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        arg(&found[0], "fixSource").as_deref(),
        Some("applicant.age + 1")
    );

    let found = with_code(
        &policy_diagnostics(vec![cell_table(
            "applicant.age",
            "(applicant.age ?? 0) + (applicant.age ?? 1)",
        )]),
        DiagnosticCode::RedundantNullish,
    );
    assert_eq!(found.len(), 2, "{found:?}");
    for d in &found {
        assert_eq!(arg(d, "fixSource"), None, "{d:?}");
    }
}

#[test]
fn fixes_splice_by_byte_offsets() {
    for text in ["café", "é🎉"] {
        let source = format!("\"{text}\" != \"x\" and applicant.target > 0");
        let found = with_code(&policy_expression(&source), DiagnosticCode::TypeMismatch);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            arg(&found[0], "fixSource"),
            Some(format!(
                "\"{text}\" != \"x\" and (applicant.target ?? 0) > 0"
            ))
        );
        assert_eq!(
            arg(&found[0], "fixOperand").as_deref(),
            Some("applicant.target")
        );

        let source = format!("\"{text}\" != \"x\" and (applicant.age ?? 0) > 1");
        let found = with_code(
            &policy_expression(&source),
            DiagnosticCode::RedundantNullish,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            arg(&found[0], "fixSource"),
            Some(format!("\"{text}\" != \"x\" and applicant.age > 1"))
        );
        assert_eq!(arg(&found[0], "fixFallback").as_deref(), Some("0"));

        let source = format!("\"{text}\" != \"x\" and (applicant.age) > 1");
        let found = with_code(
            &policy_expression(&source),
            DiagnosticCode::RedundantParentheses,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            arg(&found[0], "fixSource"),
            Some(format!("\"{text}\" != \"x\" and applicant.age > 1"))
        );
        assert_eq!(
            found[0].message,
            "unnecessary parentheses around 'applicant.age'"
        );
    }
}

#[test]
fn quick_fix_proofs_scale_with_expression_length() {
    let source = vec!["(amount ?? 0) + (1)"; 1500].join(" + ");
    let started = Instant::now();
    let diagnostics = graph_expression(&source);
    let elapsed = started.elapsed();
    eprintln!("{} bytes in {elapsed:?}", source.len());

    for (code, all, first) in [
        (
            DiagnosticCode::RedundantNullish,
            vec!["amount + (1)"; 1500].join(" + "),
            "amount + (1) + (amount ?? 0) + (1)",
        ),
        (
            DiagnosticCode::RedundantParentheses,
            vec!["(amount ?? 0) + 1"; 1500].join(" + "),
            "(amount ?? 0) + 1 + (amount ?? 0) + (1)",
        ),
    ] {
        let found = with_code(&diagnostics, code);
        assert_eq!(found.len(), 1500);
        for d in &found {
            assert_eq!(arg(d, "fixOriginal").as_deref(), Some(source.as_str()));
            assert_eq!(arg(d, "fixAll").as_deref(), Some(all.as_str()));
        }
        let fixed = arg(&found[0], "fixSource").expect("fix");
        assert!(fixed.starts_with(first), "{}", &fixed[..60]);
        assert_eq!(
            fixed.len(),
            source.len() - (source.len() - all.len()) / 1500
        );
    }
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");

    let source = vec!["target * 2"; 2000].join(" + ");
    let started = Instant::now();
    let found = with_code(&graph_expression(&source), DiagnosticCode::TypeMismatch);
    let elapsed = started.elapsed();
    assert_eq!(found.len(), 2000);
    assert!(found.iter().all(|d| d.args.contains_key("fixSource")));
    let fixed = arg(&found[0], "fixSource").expect("fix");
    assert!(
        fixed.starts_with("(target ?? 0) * 2 + target * 2"),
        "{}",
        &fixed[..60]
    );
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn nested_fallbacks_are_proven_separately() {
    let found = with_code(
        &graph_expression("-(amount ?? 0 ?? 1)"),
        DiagnosticCode::RedundantNullish,
    );
    let mut fixes: Vec<String> = found
        .iter()
        .map(|d| arg(d, "fixSource").expect("fix"))
        .collect();
    fixes.sort();
    assert_eq!(fixes, vec!["-(amount ?? 0)", "-(amount ?? 1)"]);
    for d in &found {
        assert_eq!(arg(d, "fixAll").as_deref(), Some("-(amount)"));
    }
}

fn number_schema() -> String {
    json!({
        "type": "object",
        "properties": {
            "applicant": {
                "type": "object",
                "properties": { "age": { "type": "number", "minimum": 0, "maximum": 120 } },
                "required": ["age"]
            }
        },
        "required": ["applicant"]
    })
    .to_string()
}

fn age_table() -> Value {
    json!({ "id": "dt", "name": "dt", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "inputs": [ { "id": "i0", "name": "Age", "field": "applicant.age" } ],
        "outputs": [ { "id": "o0", "name": "Rate", "field": "rate" } ],
        "rules": [ { "_id": "r1", "i0": "<= 120", "o0": "1" } ]
    } })
}

fn expression_node(id: &str, key: &str, value: &str) -> Value {
    json!({ "id": id, "name": id, "type": "expressionNode", "content": {
        "expressions": [ { "id": format!("{id}-x"), "key": key, "value": value } ],
        "passThrough": true
    } })
}

fn graph_diagnostics(nodes: Vec<Value>, edges: &[(&str, &str)]) -> Vec<Diagnostic> {
    let mut all = vec![
        json!({ "id": "in", "name": "in", "type": "inputNode", "content": { "schema": number_schema() } }),
    ];
    all.extend(nodes);
    all.push(json!({ "id": "out", "name": "out", "type": "outputNode", "content": {} }));
    let edges: Vec<Value> = edges
        .iter()
        .enumerate()
        .map(|(i, (a, b))| json!({ "id": format!("e{i}"), "sourceId": a, "targetId": b, "sourceHandle": null }))
        .collect();
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        serde_json::from_value(json!({ "nodes": all, "edges": edges })).expect("graph"),
    );
    ws.diagnostics("g")
}

#[test]
fn schema_ranges_ignore_rewritten_fields() {
    let direct = graph_diagnostics(vec![age_table()], &[("in", "dt"), ("dt", "out")]);
    assert_eq!(
        with_code(&direct, DiagnosticCode::CellCoversDomain).len(),
        1,
        "{direct:?}"
    );

    let rewritten = graph_diagnostics(
        vec![
            expression_node("calc", "applicant.age", "applicant.age + 1000"),
            age_table(),
        ],
        &[("in", "calc"), ("calc", "dt"), ("dt", "out")],
    );
    assert!(
        with_code(&rewritten, DiagnosticCode::CellCoversDomain).is_empty(),
        "{rewritten:?}"
    );
}

fn diamonds(count: usize, rewrite: bool) -> Vec<Diagnostic> {
    let mut nodes = Vec::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    let mut previous = "in".to_string();
    for i in 0..count {
        let (a, b, join) = (format!("a{i}"), format!("b{i}"), format!("j{i}"));
        let key = if rewrite && i == 0 {
            "applicant.age"
        } else {
            "applicant.seen"
        };
        nodes.push(expression_node(&a, key, "applicant.age + 1"));
        nodes.push(expression_node(&b, "applicant.seen", "applicant.age"));
        nodes.push(expression_node(&join, "applicant.seen", "applicant.age"));
        edges.push((previous.clone(), a.clone()));
        edges.push((previous.clone(), b.clone()));
        edges.push((a, join.clone()));
        edges.push((b, join.clone()));
        previous = join;
    }
    nodes.push(age_table());
    edges.push((previous, "dt".to_string()));
    edges.push(("dt".to_string(), "out".to_string()));
    let edges: Vec<(&str, &str)> = edges
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    graph_diagnostics(nodes, &edges)
}

fn summary(diagnostics: &[Diagnostic]) -> Vec<String> {
    let mut out: Vec<String> = diagnostics
        .iter()
        .map(|d| format!("{:?} {:?} {}", d.code, d.severity, d.message))
        .collect();
    out.sort();
    out
}

#[test]
fn chained_diamonds_stay_linear() {
    let started = Instant::now();
    let found = diamonds(30, false);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    let covers = vec![
        "CellCoversDomain Hint this condition accepts every possible Age value, so the cell can be empty"
            .to_string(),
    ];
    assert_eq!(summary(&found), covers);
    assert_eq!(summary(&diamonds(2, false)), covers);
    assert_eq!(
        summary(&diamonds(2, true)),
        vec!["MissingCases Hint no row matches 1 input case: Age > 120".to_string()]
    );
}

fn policy_content(blocks: Vec<Value>) -> DecisionContent {
    let mut all = vec![applicant_model()];
    all.extend(blocks);
    let policy: zen_engine::policy::PolicyDocument =
        serde_json::from_value(json!({ "blocks": all })).expect("policy");
    DecisionContent::Policy(PolicyContent(Arc::new(policy)))
}

fn gapped_table() -> Value {
    json!({ "id": "dt", "type": "decisionTable", "props": { "data": {
        "hitPolicy": "first",
        "inputs": [ { "id": "i0", "name": "Age", "field": "applicant.age" } ],
        "outputs": [ { "id": "o0", "name": "Rate", "field": "applicant.rate" } ],
        "rules": [
            { "_id": "r1", "i0": "< 18", "o0": "1" },
            { "_id": "r2", "i0": "< 10", "o0": "2" }
        ]
    } } })
}

#[tokio::test]
async fn evaluate_skips_table_checks_but_keeps_errors() {
    let table_codes = |diagnostics: &[Diagnostic]| {
        diagnostics
            .iter()
            .filter(|d| {
                matches!(
                    d.code,
                    DiagnosticCode::MissingCases | DiagnosticCode::UnreachableRule
                )
            })
            .count()
    };
    assert_eq!(table_codes(&policy_diagnostics(vec![gapped_table()])), 2);

    let loader = Arc::new(MemoryLoader::default());
    loader.add("ok", policy_content(vec![gapped_table()]));
    loader.add(
        "broken",
        policy_content(vec![
            gapped_table(),
            json!({ "id": "calc", "type": "expression", "props": { "data": { "key": "applicant.total", "value": "applicant.missing > 50" } } }),
        ]),
    );
    let engine = DecisionEngine::default().with_loader(loader);

    let result = engine
        .evaluate("ok", json!({ "applicant": { "age": 5 } }).into())
        .await
        .expect("evaluate");
    let output: Value = result.result.into();
    assert_eq!(output.pointer("/applicant/rate"), Some(&json!(1)));

    let result = engine
        .evaluate("broken", json!({ "applicant": { "age": 5 } }).into())
        .await;
    assert!(
        format!("{result:?}").contains("CompilationErrors"),
        "{result:?}"
    );
}

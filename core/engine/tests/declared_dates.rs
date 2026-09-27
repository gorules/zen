use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::policy::{Severity, Workspace};
use zen_engine::{Decision, DecisionEngine};
use zen_expression::variable::Variable;

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn graph(schema: &Value, expressions: &[(&str, &str)]) -> Value {
    let rows: Vec<Value> = expressions
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({ "id": format!("x{i}"), "key": key, "value": value }))
        .collect();
    json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": schema.to_string() })),
            node("calc", "expressionNode", json!({ "expressions": rows, "passThrough": true })),
            node("out", "outputNode", json!({}))
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "calc" },
            { "id": "e2", "sourceId": "calc", "targetId": "out" }
        ]
    })
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "since": { "type": "string", "format": "date" },
            "at": { "type": "string", "format": "date-time", "description": "moment" },
            "maybe": { "type": ["string", "null"], "format": "date" },
            "dates": { "type": "array", "items": { "type": "string", "format": "date" } },
            "items": {
                "type": "array",
                "items": { "type": "object", "properties": { "due": { "type": "string", "format": "date" } }, "required": ["due"] }
            },
            "text": { "type": "string", "format": "date", "pattern": "^2" },
            "plain": { "type": "string" }
        },
        "required": ["since", "at", "items", "text", "plain"]
    })
}

fn input() -> Value {
    json!({
        "since": "2021-05-01",
        "at": "2021-05-01T10:30:00Z",
        "maybe": null,
        "dates": ["2021-03-04"],
        "items": [{ "due": "2021-01-01" }, { "due": "2023-01-01" }],
        "text": "2021-05-01",
        "plain": "2021-05-01"
    })
}

async fn run_graph(content: Value, input: Value) -> Result<Value, String> {
    let content: zen_engine::model::GraphContent = serde_json::from_value(content).unwrap();
    let decision = Decision::from(Arc::new(content));
    decision
        .evaluate(input.into())
        .await
        .map(|r| serde_json::to_value(&r.result).unwrap())
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn declared_graph_dates_are_dates_downstream() {
    for (expression, expected) in [
        ("since > d('2020-01-01')", json!(true)),
        ("since in [d('2021-01-01')..d('2021-12-31')]", json!(true)),
        ("since.year()", json!(2021)),
        ("d(since).year()", json!(2021)),
        ("at.hour()", json!(10)),
        ("maybe", json!(null)),
        ("dates[0].month()", json!(3)),
        ("count(items, #.due > d('2022-01-01'))", json!(1)),
        ("$nodes.in.since.year()", json!(2021)),
        ("since == '2021-05-01'", json!(false)),
        ("text", json!("2021-05-01")),
        (
            "startsWith(string(since), '2021-05-01T00:00:00')",
            json!(true),
        ),
        (
            "startsWith(`joined ${since}`, 'joined 2021-05-01T00:00:00')",
            json!(true),
        ),
        ("plain", json!("2021-05-01")),
    ] {
        let out = run_graph(graph(&schema(), &[("result", expression)]), input())
            .await
            .unwrap_or_else(|e| panic!("{expression}: {e}"));
        assert_eq!(out["result"], expected, "{expression}");
    }
}

#[tokio::test]
async fn declared_graph_dates_leave_as_date_strings() {
    let out = run_graph(graph(&schema(), &[("copy", "since")]), input())
        .await
        .unwrap();
    let written = out["since"].as_str().unwrap();
    assert!(written.starts_with("2021-05-01T00:00:00"), "{written}");
    assert_eq!(out["copy"], out["since"]);
    assert_eq!(out["plain"], json!("2021-05-01"));
}

#[tokio::test]
async fn date_and_date_time_accept_any_date_like_value() {
    for format in ["date", "date-time"] {
        let schema = json!({
            "type": "object",
            "properties": { "v": { "type": "string", "format": format } },
            "required": ["v"]
        });
        for value in [
            "2021-05-01",
            "2021-05-01T10:30:00Z",
            "2021-05-01T10:30:00",
            "2021-05-01T10:30+0200",
            "20210501",
            "2021/05/01",
        ] {
            let out = run_graph(graph(&schema, &[("y", "v.year()")]), json!({ "v": value }))
                .await
                .unwrap_or_else(|e| panic!("{format} {value}: {e}"));
            assert_eq!(out["y"], json!(2021), "{format} {value}");
        }
        for value in ["hello", "2021-13-01", "Europe/Berlin"] {
            assert!(
                run_graph(graph(&schema, &[("y", "v")]), json!({ "v": value }))
                    .await
                    .is_err(),
                "{format} {value} should be rejected"
            );
        }
    }
}

#[tokio::test]
async fn dates_pass_into_child_graphs() {
    let child_schema = json!({
        "type": "object",
        "properties": { "since": { "type": "string", "format": "date" } },
        "required": ["since"]
    });
    let child = graph(&child_schema, &[("year", "since.year()")]);
    let parent = json!({
        "nodes": [
            node("in", "inputNode", json!({ "schema": schema().to_string() })),
            node("call", "decisionNode", json!({ "key": "child.json" })),
            node("out", "outputNode", json!({}))
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "call" },
            { "id": "e2", "sourceId": "call", "targetId": "out" }
        ]
    });
    let loader = MemoryLoader::default();
    loader.add(
        "child.json",
        serde_json::from_value::<DecisionContent>(child).unwrap(),
    );
    loader.add(
        "parent.json",
        serde_json::from_value::<DecisionContent>(parent).unwrap(),
    );
    let engine = DecisionEngine::default().with_loader(Arc::new(loader));
    let out = engine
        .evaluate("parent.json", Variable::from(input()))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&out.result).unwrap()["year"],
        json!(2021)
    );
}

fn graph_errors(expression: &str) -> Vec<String> {
    let mut ws = Workspace::new();
    ws.set_document(
        "g",
        serde_json::from_value(graph(&schema(), &[("result", expression)])).unwrap(),
    );
    ws.diagnostics("g")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect()
}

#[test]
fn graph_checker_types_declared_dates_as_dates() {
    for expression in [
        "since > d('2020-01-01')",
        "since.year()",
        "at.hour()",
        "count(items, #.due > d('2022-01-01'))",
        "startsWith(text, '2')",
        "startsWith(plain, '2')",
    ] {
        let errors = graph_errors(expression);
        assert!(errors.is_empty(), "{expression}: {errors:?}");
    }
    for expression in [
        "startsWith(since, '2')",
        "text > d('2020-01-01')",
        "plain.year()",
    ] {
        assert!(
            !graph_errors(expression).is_empty(),
            "{expression} should be an error"
        );
    }
}

fn policy() -> Value {
    json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "applicant",
                "properties": [
                    { "id": "p1", "name": "birthDate", "type": "date", "array": false, "optional": false },
                    { "id": "p2", "name": "visits", "type": "date", "array": true, "optional": true },
                    { "id": "p3", "name": "lastSeen", "type": "date", "array": false, "optional": true },
                    { "id": "p4", "name": "employer", "type": "relationship", "target": "company", "array": false, "optional": false },
                    { "id": "p5", "name": "name", "type": "string", "array": false, "optional": false }
                ]
            }}},
            { "id": "dm2", "type": "dataModel", "props": { "data": {
                "name": "company",
                "properties": [
                    { "id": "c1", "name": "founded", "type": "date", "array": false, "optional": false }
                ]
            }}},
            { "id": "dmg", "type": "dataModel", "props": { "data": {
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "asOf", "type": "date", "array": false, "optional": false }
                ]
            }}},
            { "id": "e1", "type": "expression", "props": { "data": {
                "key": "applicant.born90s", "value": "applicant.birthDate in [d('1990-01-01')..d('1999-12-31')]"
            }}},
            { "id": "e2", "type": "expression", "props": { "data": {
                "key": "applicant.age", "value": "asOf.diff(applicant.birthDate, 'year')"
            }}},
            { "id": "e3", "type": "expression", "props": { "data": {
                "key": "applicant.visitCount", "value": "count(applicant.visits ?? [], #.year() == 2024)"
            }}},
            { "id": "e4", "type": "expression", "props": { "data": {
                "key": "applicant.oldEmployer", "value": "applicant.employer.founded < d('2000-01-01')"
            }}},
            { "id": "e5", "type": "expression", "props": { "data": {
                "key": "applicant.seen", "value": "applicant.lastSeen == null"
            }}},
            { "id": "e6", "type": "expression", "props": { "data": {
                "key": "applicant.birthText", "value": "string(applicant.birthDate)"
            }}}
        ]
    })
}

#[tokio::test]
async fn declared_policy_dates_are_dates_downstream() {
    let loader = MemoryLoader::default();
    loader.add(
        "p.json",
        serde_json::from_value::<DecisionContent>(policy()).unwrap(),
    );
    let engine = DecisionEngine::default().with_loader(Arc::new(loader));
    assert!(engine.compile().is_empty());

    let input = json!({
        "asOf": "2026-01-01",
        "applicant": {
            "birthDate": "1990-05-01",
            "visits": ["2024-02-01", "2023-02-01T10:00:00Z", "2024-12-31"],
            "lastSeen": null,
            "employer": { "founded": "1998-01-01" },
            "name": "Ann"
        }
    });
    let out = engine
        .evaluate("p.json", Variable::from(input))
        .await
        .unwrap();
    let applicant = serde_json::to_value(&out.result).unwrap()["applicant"].clone();
    assert_eq!(applicant["born90s"], json!(true));
    assert_eq!(applicant["age"], json!(35));
    assert_eq!(applicant["visitCount"], json!(2));
    assert_eq!(applicant["oldEmployer"], json!(true));
    assert_eq!(applicant["seen"], json!(true));
    assert_eq!(applicant["name"], json!("Ann"));
    assert!(applicant["birthText"]
        .as_str()
        .unwrap()
        .starts_with("1990-05-01T00:00:00"));

    let invalid =
        json!({ "asOf": "2026-01-01", "applicant": { "birthDate": "hello", "name": "Ann" } });
    assert!(engine
        .evaluate("p.json", Variable::from(invalid))
        .await
        .is_err());
}

#[test]
fn policy_checker_types_declared_dates_as_dates() {
    let mut ws = Workspace::new();
    ws.set_document("p", serde_json::from_value(policy()).unwrap());
    let errors: Vec<String> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[tokio::test]
async fn policy_input_skeleton_is_valid_for_dates() {
    let mut ws = Workspace::new();
    ws.set_document("p", serde_json::from_value(policy()).unwrap());
    let skeleton = ws.input_skeleton(&zen_engine::policy::ScopeRequest::for_policy("p"));
    assert_eq!(skeleton["applicant"]["birthDate"], json!("2000-01-01"));
    assert_eq!(skeleton["asOf"], json!("2000-01-01"));

    ws.set_document(
        "g",
        serde_json::from_value(graph(&schema(), &[("result", "since")])).unwrap(),
    );
    let skeleton = ws.input_skeleton(&zen_engine::policy::ScopeRequest::for_policy("g"));
    assert_eq!(skeleton["since"], json!("2000-01-01"));
}

mod compiled_default;
mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::{DecisionContent, GraphContent};
use zen_engine::{Decision, EvaluationOptions};
use zen_expression::Variable;

struct Case;

impl Case {
    fn graph(nodes: Vec<Value>, imports: Vec<&str>) -> GraphContent {
        let edges: Vec<Value> = nodes
            .windows(2)
            .enumerate()
            .map(|(i, pair)| json!({ "id": format!("e{i}"), "sourceId": pair[0]["id"], "targetId": pair[1]["id"] }))
            .collect();
        serde_json::from_value(json!({ "nodes": nodes, "edges": edges, "imports": imports }))
            .expect("graph")
    }

    fn input(schema: Value) -> Value {
        json!({ "id": "in", "name": "request", "type": "inputNode", "content": { "schema": schema.to_string() } })
    }

    fn expression(fields: &[(&str, &str)]) -> Value {
        let expressions: Vec<Value> = fields
            .iter()
            .enumerate()
            .map(|(i, (key, value))| json!({ "id": format!("x{i}"), "key": key, "value": value }))
            .collect();
        json!({ "id": "calc", "name": "calc", "type": "expressionNode", "content": { "expressions": expressions, "passThrough": true, "inputField": null, "outputPath": null, "executionMode": "single" } })
    }

    fn output(schema: Option<Value>) -> Value {
        let content = match schema {
            Some(schema) => json!({ "schema": schema.to_string() }),
            None => json!({}),
        };
        json!({ "id": "out", "name": "response", "type": "outputNode", "content": content })
    }

    fn render(result: Result<Variable, String>) -> Value {
        match result {
            Ok(value) => json!({ "ok": Built::normalized(value.to_value()) }),
            Err(error) => json!({ "error": error }),
        }
    }

    async fn compare(decision: &Decision, rows: Vec<Value>) {
        std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-03-20T10:15:30Z");
        let mut expected = Vec::new();
        for row in &rows {
            let result = decision
                .evaluate(Variable::from(row.clone()))
                .await
                .map(|r| r.result)
                .map_err(|e| serde_json::to_string(&e).unwrap_or_default());
            expected.push(Self::render(result));
        }
        let mut representations = 0;
        for built in [Built::new(&rows), Built::nested(&rows)]
            .into_iter()
            .flatten()
        {
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            let output = decision
                .evaluate_columns(&columns, EvaluationOptions::default())
                .await;
            representations += 1;
            assert_eq!(output.paths().hosted, 0, "rows left the columnar plan");
            for (row, want) in expected.iter().enumerate() {
                let got = Self::render(match &output.errors[row] {
                    Some(error) => Err(serde_json::to_string(error.as_ref()).unwrap_or_default()),
                    None => Ok(output.row(row)),
                });
                assert_eq!(&got, want, "row {row}: {}", rows[row]);
            }
        }
        assert!(representations > 0, "no columnar representation built");
    }
}

#[tokio::test]
async fn date_schemas_match_row_evaluation_in_one_columnar_batch() {
    let schema = json!({
        "type": "object",
        "required": ["at"],
        "properties": {
            "at": { "type": "string", "format": "date-time" },
            "day": { "anyOf": [{ "type": "string", "format": "date" }, { "type": "null" }] },
            "customer": { "type": "object", "properties": { "born": { "type": "string", "format": "date" }, "name": { "type": "string" } } },
            "count": { "type": "integer" }
        }
    });
    let decision = Decision::from(Case::graph(
        vec![
            Case::input(schema),
            Case::expression(&[
                ("kind", "type(at)"),
                ("hour", "at.hour()"),
                ("dayKind", "type(day)"),
                ("born", "customer.born == null ? null : year(customer.born)"),
                ("bornKind", "type(customer.born)"),
            ]),
            Case::output(None),
        ],
        vec![],
    ));
    let rows = vec![
        json!({ "at": "2024-01-31T10:00:00Z", "day": "2024-02-29", "customer": { "born": "1990-05-01", "name": "a" }, "count": 1 }),
        json!({ "at": "2024-01-31T10:00:00+02:00", "day": null, "customer": { "name": "b" }, "count": 2 }),
        json!({ "at": "not a date", "day": "2024-02-29", "customer": { "born": "1990-05-01", "name": "c" }, "count": 3 }),
        json!({ "at": "2024-01-31T23:30:00-05:00", "day": "2023-02-29", "customer": { "born": "1990-05-01", "name": "d" }, "count": 4 }),
        json!({ "day": "2024-01-01", "customer": { "born": "1990-05-01", "name": "e" }, "count": 5 }),
        json!({ "at": "2024-01-31T10:00", "customer": { "born": "31/01/1990", "name": "f" }, "count": 6 }),
        json!({ "at": "2024-01-31T10:00:00Z", "day": "2024", "customer": { "born": "2000", "name": "g" }, "count": 7.5 }),
        json!({ "at": "2024-01-31", "day": "20240131", "customer": { "born": "1990-05-01", "name": "h" }, "count": 8 }),
    ];
    Case::compare(&decision, rows).await;
}

#[tokio::test]
async fn dates_inside_lists_and_untouched_dates_match_row_evaluation() {
    let schema = json!({
        "type": "object",
        "properties": {
            "visits": { "type": "array", "items": { "type": "string", "format": "date" } },
            "plain": { "type": "string" }
        }
    });
    let decision = Decision::from(Case::graph(
        vec![
            Case::input(schema),
            Case::expression(&[("first", "type(visits[0])"), ("count", "len(visits)")]),
            Case::output(None),
        ],
        vec![],
    ));
    let rows = vec![
        json!({ "visits": ["2024-01-01", "2024-02-01"], "plain": "2024-01-01" }),
        json!({ "visits": ["2024-03-01"], "plain": "x" }),
        json!({ "visits": ["bad"], "plain": "y" }),
        json!({ "visits": [], "plain": "z" }),
    ];
    Case::compare(&decision, rows).await;
}

#[tokio::test]
async fn dictionary_schemas_match_row_evaluation_in_one_columnar_batch() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add(
        "dicts",
        serde_json::from_value::<DecisionContent>(json!({
            "imports": [],
            "blocks": [{ "id": "dict1", "type": "dictionary", "props": { "data": { "name": "customerTier",
                "entries": [{ "id": "e0", "value": "VIP", "label": "v" }, { "id": "e1", "value": "STD", "label": "s" }] } } }]
        }))
        .expect("dictionary"),
    );
    let schema = json!({ "type": "object", "properties": { "tier": { "$dictionary": "customerTier" }, "since": { "type": "string", "format": "date" } } });
    let decision = Decision::from(Case::graph(
        vec![
            Case::input(schema),
            Case::expression(&[("vip", "tier == 'VIP'"), ("sinceKind", "type(since)")]),
            Case::output(None),
        ],
        vec!["dicts"],
    ))
    .with_loader(loader);
    let rows = vec![
        json!({ "tier": "VIP", "since": "2024-01-01" }),
        json!({ "tier": "STD", "since": "2024-02-30" }),
        json!({ "tier": "GOLD", "since": "2024-01-01" }),
        json!({ "tier": "VIP" }),
        json!({ "since": "2024-05-05" }),
    ];
    Case::compare(&decision, rows).await;
}

#[tokio::test]
async fn output_schemas_over_mixed_columns_match_row_evaluation() {
    let schema = json!({
        "type": "object",
        "required": ["count", "status"],
        "properties": {
            "count": { "type": "integer", "minimum": 0 },
            "ratio": { "type": "number" },
            "status": { "enum": ["approved", "review"] },
            "flag": { "type": "boolean" },
            "label": { "type": "string", "maxLength": 3 }
        }
    });
    let decision = Decision::from(Case::graph(
        vec![
            json!({ "id": "in", "name": "request", "type": "inputNode", "content": {} }),
            json!({ "id": "calc", "name": "calc", "type": "expressionNode", "content": {
                "expressions": [
                    { "id": "a", "key": "count", "value": "kind == 'a' ? 1 : (kind == 'b' ? 'x' : (kind == 'c' ? 2.5 : (kind == 'd' ? -1 : 7)))" },
                    { "id": "b", "key": "ratio", "value": "kind == 'e' ? 'no' : 0.5" },
                    { "id": "c", "key": "status", "value": "kind == 'f' ? 'other' : 'approved'" },
                    { "id": "d", "key": "flag", "value": "kind == 'g' ? 1 : true" },
                    { "id": "e", "key": "label", "value": "kind == 'h' ? 'long text' : 'ok'" }
                ],
                "passThrough": false, "inputField": null, "outputPath": null, "executionMode": "single"
            } }),
            Case::output(Some(schema)),
        ],
        vec![],
    ));
    let rows: Vec<Value> = ["a", "b", "c", "d", "e", "f", "g", "h", "z", "a", "z"]
        .iter()
        .map(|kind| json!({ "kind": kind }))
        .collect();
    Case::compare(&decision, rows).await;
}

#[tokio::test]
async fn unmodelled_property_keywords_match_row_evaluation() {
    let schema = json!({
        "type": "object",
        "required": ["email", "code"],
        "properties": {
            "email": { "type": "string", "format": "email" },
            "code": { "type": "string", "pattern": "^[A-Z]{2}[0-9]+$" },
            "step": { "type": "number", "multipleOf": 0.5 },
            "contact": { "oneOf": [{ "type": "string" }, { "type": "integer", "minimum": 1 }] },
            "customer": { "type": "object", "properties": { "vat": { "type": "string", "pattern": "^DE" } } }
        }
    });
    let decision = Decision::from(Case::graph(
        vec![
            Case::input(schema),
            Case::expression(&[("ok", "true")]),
            Case::output(None),
        ],
        vec![],
    ));
    let rows = vec![
        json!({ "email": "a@b.co", "code": "DE12", "step": 1.5, "contact": "x", "customer": { "vat": "DE1" } }),
        json!({ "email": "not-an-email", "code": "DE12", "step": 1.5, "contact": 2, "customer": { "vat": "DE1" } }),
        json!({ "email": "a@b.co", "code": "de12", "step": 1.5, "contact": 2 }),
        json!({ "email": "a@b.co", "code": "DE12", "step": 1.25, "contact": 3 }),
        json!({ "email": "a@b.co", "code": "DE12", "step": 2, "contact": 0 }),
        json!({ "email": "a@b.co", "code": "DE12", "contact": 1.5, "customer": { "vat": "FR1" } }),
        json!({ "email": "c@d.org", "code": "AT9", "step": 0.5, "contact": "y", "customer": { "vat": "DE9" } }),
        json!({ "code": "DE12" }),
    ];
    Case::compare(&decision, rows).await;
}

#[tokio::test]
async fn output_schemas_over_constant_columns_match_row_evaluation() {
    for (field, value) in [
        ("count", "-1"),
        ("count", "1.5"),
        ("count", "3"),
        ("ratio", "'0.5'"),
        ("status", "'other'"),
        ("status", "'review'"),
        ("flag", "1"),
        ("label", "'long text'"),
    ] {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 0 },
                "ratio": { "type": "number" },
                "status": { "enum": ["approved", "review"] },
                "flag": { "type": "boolean" },
                "label": { "type": "string", "maxLength": 3 }
            }
        });
        let decision = Decision::from(Case::graph(
            vec![
                json!({ "id": "in", "name": "request", "type": "inputNode", "content": {} }),
                json!({ "id": "calc", "name": "calc", "type": "expressionNode", "content": {
                    "expressions": [{ "id": "a", "key": field, "value": value }, { "id": "b", "key": "seen", "value": "kind" }],
                    "passThrough": false, "inputField": null, "outputPath": null, "executionMode": "single"
                } }),
                Case::output(Some(schema)),
            ],
            vec![],
        ));
        let rows: Vec<Value> = (0..8).map(|i| json!({ "kind": format!("k{i}") })).collect();
        Case::compare(&decision, rows).await;
    }
}

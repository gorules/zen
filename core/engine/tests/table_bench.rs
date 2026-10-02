use std::time::Instant;

use serde_json::{json, Map, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{DiagnosticCode, Workspace};

fn graph(table: Value) -> DecisionContent {
    let schema = json!({
        "type": "object",
        "properties": {
            "amount": { "type": "number" },
            "country": { "type": "string" },
            "segment": { "type": "string" },
            "vip": { "type": "boolean" },
            "age": { "type": "number" },
            "httpCode": { "type": "number" },
            "appCode": { "type": "string" }
        },
        "required": ["amount", "country", "segment", "vip", "age", "httpCode", "appCode"]
    });
    serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "request", "type": "inputNode", "content": { "schema": schema.to_string() } },
            table,
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "dt" },
            { "id": "e2", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .expect("graph")
}

fn table(inputs: Value, outputs: Value, rules: Vec<Value>) -> Value {
    json!({ "id": "dt", "name": "bench", "type": "decisionTableNode", "content": {
        "hitPolicy": "first", "inputs": inputs, "outputs": outputs, "rules": rules, "passThrough": true
    } })
}

const COUNTRIES: [&str; 6] = [
    "\"US\", \"CA\"",
    "\"MX\"",
    "\"GB\", \"IE\"",
    "\"DE\", \"AT\", \"CH\"",
    "\"FR\"",
    "\"JP\"",
];
const SEGMENTS: [&str; 4] = ["\"retail\"", "\"sme\"", "\"corporate\"", "\"public\""];

fn grid(rows: usize, compressible: bool) -> Value {
    let inputs = json!([
        { "id": "a", "name": "Amount", "field": "amount" },
        { "id": "c", "name": "Country", "field": "country" },
        { "id": "s", "name": "Segment", "field": "segment" },
        { "id": "v", "name": "VIP", "field": "vip" },
        { "id": "g", "name": "Age", "field": "age" }
    ]);
    let outputs = json!([ { "id": "r", "name": "Rate", "field": "rate" } ]);
    let mut rules = Vec::with_capacity(rows);
    let ages = [(18, 25), (25, 35), (35, 50), (50, 65), (65, 120)];
    'outer: for band in 0.. {
        for (ci, country) in COUNTRIES.iter().enumerate() {
            for (si, segment) in SEGMENTS.iter().enumerate() {
                for vip in [true, false] {
                    for (gi, (lo, hi)) in ages.iter().enumerate() {
                        if rules.len() >= rows {
                            break 'outer;
                        }
                        let rate = if compressible {
                            format!("{}", band as f64 * 0.001)
                        } else {
                            format!(
                                "{}",
                                (band * 997 + ci * 131 + si * 31 + gi * 7 + vip as usize) % 1000
                            )
                        };
                        let mut rule = Map::new();
                        rule.insert("_id".into(), json!(format!("r{}", rules.len())));
                        rule.insert(
                            "a".into(),
                            json!(format!(">= {} and < {}", band * 1000, (band + 1) * 1000)),
                        );
                        rule.insert("c".into(), json!(country));
                        rule.insert("s".into(), json!(segment));
                        rule.insert("v".into(), json!(vip.to_string()));
                        rule.insert("g".into(), json!(format!("[{lo}..{hi})")));
                        rule.insert("r".into(), json!(rate));
                        rules.push(Value::Object(rule));
                    }
                }
            }
        }
    }
    table(inputs, outputs, rules)
}

fn lookup(rows: usize) -> Value {
    let inputs = json!([
        { "id": "h", "name": "HttpCode", "field": "httpCode" },
        { "id": "p", "name": "AppCode", "field": "appCode" }
    ]);
    let outputs = json!([
        { "id": "o", "name": "Reason", "field": "reasonCode" },
        { "id": "x", "name": "Serviceable", "field": "serviceable" }
    ]);
    let rules = (0..rows)
        .map(|i| {
            let code = ["200", "400", "500"][i % 3];
            json!({
                "_id": format!("r{i}"),
                "h": code,
                "p": format!("'SERV_{i:05}'"),
                "o": format!("'R_{i:05}'"),
                "x": if i % 5 == 0 { "true" } else { "false" }
            })
        })
        .collect();
    table(inputs, outputs, rules)
}

fn measure(label: &str, table: Value) {
    let content = graph(table);
    let mut ws = Workspace::new();
    ws.set_document("g", content);
    let start = Instant::now();
    let diagnostics = ws.diagnostics("g");
    let elapsed = start.elapsed();
    let count = |code: DiagnosticCode| diagnostics.iter().filter(|d| d.code == code).count();
    println!(
        "{label:<28} live {:>9.1} ms  missing={} compress={} unreachable={} duplicate={} incomplete={} total={}",
        elapsed.as_secs_f64() * 1000.0,
        count(DiagnosticCode::MissingCases),
        count(DiagnosticCode::CompressibleTable),
        count(DiagnosticCode::UnreachableRule),
        count(DiagnosticCode::DuplicateRule),
        count(DiagnosticCode::TableChecksIncomplete),
        diagnostics.len()
    );
}

fn chained(tables: usize, rows: usize) -> DecisionContent {
    let schema = json!({
        "type": "object",
        "properties": {
            "amount": { "type": "number" }, "country": { "type": "string" },
            "segment": { "type": "string" }, "vip": { "type": "boolean" }, "age": { "type": "number" }
        },
        "required": ["amount", "country", "segment", "vip", "age"]
    });
    let mut nodes = vec![
        json!({ "id": "in", "name": "request", "type": "inputNode", "content": { "schema": schema.to_string() } }),
    ];
    let mut edges = Vec::new();
    let mut previous = "in".to_string();
    for t in 0..tables {
        let mut node = grid(rows, t % 2 == 0);
        let id = format!("dt{t}");
        node["id"] = json!(id);
        node["name"] = json!(format!("table{t}"));
        node["content"]["outputs"] =
            json!([ { "id": "r", "name": "Rate", "field": format!("rate{t}") } ]);
        nodes.push(node);
        edges.push(json!({ "id": format!("e{t}"), "sourceId": previous, "targetId": id }));
        previous = id;
    }
    nodes.push(json!({ "id": "out", "name": "out", "type": "outputNode", "content": {} }));
    edges.push(json!({ "id": "eout", "sourceId": previous, "targetId": "out" }));
    serde_json::from_value(json!({ "nodes": nodes, "edges": edges })).expect("graph")
}

#[test]
#[ignore]
fn project_benchmark() {
    let (files, tables, rows) = (300, 10, 50);
    let mut ws = Workspace::new();
    for f in 0..files {
        ws.set_document(format!("g{f}"), chained(tables, rows));
    }
    let start = Instant::now();
    let all = ws.all_diagnostics();
    println!(
        "project {files} files x {tables} tables x {rows} rows {:>9.1} ms  diagnostics={}",
        start.elapsed().as_secs_f64() * 1000.0,
        all.len()
    );
    ws.set_document("g0", chained(tables, rows));
    let start = Instant::now();
    let _ = ws.all_diagnostics();
    println!(
        "project after one file edit             {:>9.1} ms",
        start.elapsed().as_secs_f64() * 1000.0
    );
}

#[test]
#[ignore]
fn table_benchmark() {
    for rows in [200, 1_000, 2_000, 5_000, 10_000] {
        measure(&format!("grid {rows}"), grid(rows, false));
        measure(&format!("grid compressible {rows}"), grid(rows, true));
        measure(&format!("lookup {rows}"), lookup(rows));
    }
}

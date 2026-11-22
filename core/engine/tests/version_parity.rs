use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EngineVersion, EvaluationOptions, Variable};

fn graph(node: Value) -> Value {
    json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            node,
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "node", "type": "edge"},
            {"id": "e2", "sourceId": "node", "targetId": "out", "type": "edge"}
        ]
    })
}

fn expressions(pairs: &[(&str, &str)]) -> Value {
    let list: Vec<Value> = pairs
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({"id": format!("x{i}"), "key": key, "value": value}))
        .collect();
    graph(
        json!({"id": "node", "name": "node", "type": "expressionNode", "content": {"expressions": list}}),
    )
}

fn table(policy: &str, field: &str, cells: &[&str]) -> Value {
    let rules: Vec<Value> = cells
        .iter()
        .enumerate()
        .map(|(i, cell)| json!({"_id": format!("r{i}"), "i0": cell, "o0": i.to_string()}))
        .collect();
    graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": policy,
            "inputs": [{"id": "i0", "field": field}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": rules
        }}),
    )
}

#[derive(Clone, Copy, Debug)]
enum Path {
    Repeated,
    Compiled,
    Batch,
}

fn outcomes(version: EngineVersion, path: Path, graph: &Value, input: &Value) -> Vec<Value> {
    let content: DecisionContent = serde_json::from_value(graph.clone()).unwrap();
    let engine = DecisionEngine::default().with_version(version);
    let mut decision = engine.create_decision(Arc::new(content)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let one =
        |decision: &zen_engine::Decision| {
            runtime
                .block_on(decision.evaluate_with_opts(
                    Variable::from(input.clone()),
                    EvaluationOptions::default(),
                ))
                .map(|response| response.result.to_value())
                .unwrap_or_else(|error| json!({"error": error.to_string()}))
        };
    match path {
        Path::Repeated => (0..3).map(|_| one(&decision)).collect(),
        Path::Compiled => {
            decision.compile();
            (0..3).map(|_| one(&decision)).collect()
        }
        Path::Batch => {
            let rows = vec![Variable::from(input.clone()); 3];
            runtime
                .block_on(decision.evaluate_batch(&rows, EvaluationOptions::default()))
                .into_iter()
                .map(|row| {
                    row.map(|response| response.result.to_value())
                        .unwrap_or_else(|error| json!({"error": error.to_string()}))
                })
                .collect()
        }
    }
}

fn same_on_every_path(graph: &Value, input: &Value, want: Value) {
    for path in [Path::Repeated, Path::Compiled, Path::Batch] {
        for version in [EngineVersion::V1, EngineVersion::V2] {
            for (call, got) in outcomes(version, path, graph, input)
                .into_iter()
                .enumerate()
            {
                assert_eq!(got, want, "{version:?} {path:?} call {call}");
            }
        }
    }
}

#[test]
fn a_field_can_hold_the_fields_built_so_far() {
    same_on_every_path(
        &expressions(&[("a", "1"), ("b", "$")]),
        &json!({}),
        json!({"a": 1, "b": {"a": 1}}),
    );
    same_on_every_path(
        &expressions(&[("r", "1"), ("s", "[$]")]),
        &json!({}),
        json!({"r": 1, "s": [{"r": 1}]}),
    );
    same_on_every_path(
        &expressions(&[("a.c", "1"), ("a.b", "$.a")]),
        &json!({}),
        json!({"a": {"b": {"c": 1}, "c": 1}}),
    );
    same_on_every_path(
        &expressions(&[("big", "false"), ("res", "$root")]),
        &json!({}),
        json!({"big": false, "res": {"$": {"big": false}}}),
    );
}

#[test]
fn a_field_read_earlier_keeps_its_value_when_the_branch_grows() {
    same_on_every_path(
        &expressions(&[("a.b", "1"), ("n", "$.a"), ("a.c", "2")]),
        &json!({}),
        json!({"a": {"b": 1, "c": 2}, "n": {"b": 1}}),
    );
    same_on_every_path(
        &expressions(&[("a.b.c", "1"), ("x", "[$.a.b]"), ("a.b.d", "2")]),
        &json!({}),
        json!({"a": {"b": {"c": 1, "d": 2}}, "x": [{"c": 1}]}),
    );
}

#[test]
fn a_date_matches_every_text_cell_naming_it() {
    let input = json!({"x": "2024-01-01"});
    for _ in 0..20 {
        same_on_every_path(
            &table("first", "d(x)", &["'2024-01-01'", "'2024'"]),
            &input,
            json!({"out": 0}),
        );
        same_on_every_path(
            &table("first", "d(x)", &["'a'", "'2024-01'", "'2024-01-01'"]),
            &input,
            json!({"out": 1}),
        );
        same_on_every_path(
            &table("collect", "d(x)", &["'2024-01-01'", "'2024-01'", ""]),
            &input,
            json!([{"out": 0}, {"out": 1}, {"out": 2}]),
        );
        same_on_every_path(
            &table("first", "d(x)", &["'2023-12-31', '2024'", "'2024-01-01'"]),
            &input,
            json!({"out": 0}),
        );
    }
}

fn chained(nodes: Vec<Value>) -> Value {
    let mut all = vec![json!({"id": "in", "name": "in", "type": "inputNode"})];
    all.extend(nodes);
    all.push(json!({"id": "out", "name": "out", "type": "outputNode"}));
    let ids: Vec<String> = all
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect();
    let edges: Vec<Value> = ids
        .windows(2)
        .map(|w| json!({"id": format!("{}-{}", w[0], w[1]), "sourceId": w[0], "targetId": w[1], "type": "edge"}))
        .collect();
    json!({"nodes": all, "edges": edges})
}

fn batch_rows_match_single_rows(graph: &Value, rows: &[Value]) {
    let content: DecisionContent = serde_json::from_value(graph.clone()).unwrap();
    let decision = DecisionEngine::default()
        .with_version(EngineVersion::V2)
        .create_decision(Arc::new(content))
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let inputs: Vec<Variable> = rows.iter().cloned().map(Variable::from).collect();
    let batch = runtime.block_on(decision.evaluate_batch(&inputs, EvaluationOptions::default()));
    for (at, (row, got)) in rows.iter().zip(batch).enumerate() {
        let got = got
            .map(|response| response.result.to_value())
            .unwrap_or_else(|error| json!({"error": error.to_string()}));
        let want = outcomes(EngineVersion::V1, Path::Repeated, graph, row).remove(0);
        assert_eq!(got, want, "row {at}");
    }
}

#[test]
fn a_batch_row_reads_earlier_nodes_beside_its_own_fields() {
    batch_rows_match_single_rows(
        &chained(vec![
            json!({"id": "calc", "name": "calc", "type": "expressionNode", "content": {
                "expressions": [
                    {"id": "a", "key": "q", "value": "$"},
                    {"id": "b", "key": "n", "value": "$nodes.in"},
                    {"id": "c", "key": "m", "value": "$nodes.in.x + 1"}
                ]
            }}),
        ]),
        &[json!({"x": 1}), json!({"x": 2})],
    );
}

#[test]
fn a_batch_row_sees_a_field_another_node_replaced() {
    let graph = |read: &str| {
        chained(vec![
            json!({"id": "first", "name": "first", "type": "expressionNode", "content": {
                "expressions": [{"id": "a", "key": "obj", "value": "{q: 1}"}],
                "passThrough": true
            }}),
            json!({"id": "second", "name": "second", "type": "expressionNode", "content": {
                "expressions": [{"id": "b", "key": "x", "value": read}],
                "inputField": "obj"
            }}),
        ])
    };
    let rows = [json!({"obj": {"p": 1}}), json!({"obj": "s"}), json!({})];
    batch_rows_match_single_rows(&graph("zz ?? 1"), &rows);
    batch_rows_match_single_rows(&graph("p ?? 9"), &rows);
    batch_rows_match_single_rows(&graph("q"), &rows);
}

fn wide_table(policy: &str, inputs: Value, rules: Vec<Value>) -> Value {
    graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": policy,
            "inputs": inputs,
            "outputs": [{"id": "o0", "field": "v"}],
            "rules": rules
        }}),
    )
}

#[test]
fn a_table_cell_reads_the_last_tested_value_however_many_rules_there_are() {
    for padding in [0, 3, 7, 20] {
        let mut rules = vec![json!({"_id": "r0", "i0": "", "o0": "$"})];
        for at in 0..padding {
            rules.push(
                json!({"_id": format!("p{at}"), "i0": format!("\"q{at}\""), "o0": at.to_string()}),
            );
        }
        same_on_every_path(
            &wide_table("first", json!([{"id": "i0", "field": "s"}]), rules.clone()),
            &json!({"s": "x"}),
            json!({}),
        );
        rules.rotate_left(1);
        same_on_every_path(
            &wide_table("first", json!([{"id": "i0", "field": "s"}]), rules),
            &json!({"s": "x"}),
            match padding {
                0 => json!({}),
                _ => json!({"v": "x"}),
            },
        );
    }
}

#[test]
fn a_table_column_can_test_the_value_of_the_column_before_it() {
    let rules = vec![
        json!({"_id": "a", "i0": "> 1", "i1": "> 10", "o0": "'big'"}),
        json!({"_id": "b", "i0": "", "i1": "", "o0": "'other'"}),
    ];
    let inputs = json!([{"id": "i0", "field": "x"}, {"id": "i1", "field": "$"}]);
    same_on_every_path(
        &wide_table("first", inputs.clone(), rules.clone()),
        &json!({"x": 20}),
        json!({"v": "big"}),
    );
    same_on_every_path(
        &wide_table("first", inputs, rules),
        &json!({"x": 5}),
        json!({"v": "other"}),
    );
}

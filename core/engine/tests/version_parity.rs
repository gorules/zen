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

#[test]
fn a_policy_date_input_is_read_in_the_evaluation_zone() {
    let policy: zen_engine::policy::PolicyDocument = serde_json::from_value(json!({
        "blocks": [
            {"id": "dm", "type": "dataModel", "props": {"data": {"name": "c", "properties": [
                {"id": "p1", "name": "d", "type": "date", "array": false, "optional": false},
                {"id": "p2", "name": "s", "type": "string", "array": false, "optional": false}
            ]}}, "children": []},
            {"id": "e1", "type": "expression", "props": {"data": {
                "key": "c.typed", "value": "c.d == d('2025-01-01')"
            }}},
            {"id": "e2", "type": "expression", "props": {"data": {
                "key": "c.text", "value": "d(c.s) == d('2025-01-01')"
            }}},
            {"id": "e3", "type": "expression", "props": {"data": {
                "key": "c.shown", "value": "d(c.d).format('%Y-%m-%d %H:%M %z')"
            }}}
        ]
    }))
    .unwrap();
    let mut workspace = zen_engine::Workspace::new().with_version(EngineVersion::V2);
    workspace.set_policy("policy", policy);
    let request = zen_engine::policy::EvaluateRequest {
        policy_path: Arc::from("policy"),
        input: Variable::from(json!({"c": {"d": "2025-01-01", "s": "2025-01-01"}})),
        goals: Vec::new(),
        trace: zen_engine::trace::TraceFormat::Off,
    };
    for zone in [
        "Europe/Berlin",
        "America/St_Johns",
        "Pacific/Kiritimati",
        "UTC",
    ] {
        let options = EvaluationOptions {
            now: zen_engine::context::parse_instant("2025-08-19T16:55:02.078Z"),
            timezone: zen_engine::context::parse_timezone(zone),
            ..Default::default()
        };
        let single = workspace.evaluate_pinned(&request, &options).unwrap();
        let batch = workspace
            .evaluate_batch_pinned(&[request.clone(), request.clone()], &options)
            .remove(1)
            .unwrap();
        for output in [single.output.to_value(), batch.output.to_value()] {
            assert_eq!(output["c"]["typed"], json!(true), "{zone} {output}");
            assert_eq!(output["c"]["text"], json!(true), "{zone} {output}");
            assert!(
                output["c"]["shown"]
                    .as_str()
                    .unwrap()
                    .starts_with("2025-01-01 00:00"),
                "{zone} {output}"
            );
        }
    }
}

#[test]
fn a_graph_date_input_is_read_in_the_evaluation_zone() {
    let schema =
        json!({"type": "object", "properties": {"x": {"type": "string", "format": "date"}}})
            .to_string();
    let graph = json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode", "content": {"schema": schema}},
            {"id": "node", "name": "node", "type": "expressionNode", "content": {"expressions": [
                {"id": "a", "key": "typed", "value": "x == d('2025-01-01')"},
                {"id": "b", "key": "shown", "value": "d(x).format('%Y-%m-%d %H:%M')"}
            ]}},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "node", "type": "edge"},
            {"id": "e2", "sourceId": "node", "targetId": "out", "type": "edge"}
        ]
    });
    let content: DecisionContent = serde_json::from_value(graph).unwrap();
    let mut decision = DecisionEngine::default()
        .with_version(EngineVersion::V2)
        .create_decision(Arc::new(content))
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for zone in [
        "Europe/Berlin",
        "America/St_Johns",
        "Pacific/Kiritimati",
        "UTC",
    ] {
        let options = EvaluationOptions {
            now: zen_engine::context::parse_instant("2025-08-19T16:55:02.078Z"),
            timezone: zen_engine::context::parse_timezone(zone),
            ..Default::default()
        };
        let input = || Variable::from(json!({"x": "2025-01-01"}));
        let mut outputs = Vec::new();
        for _ in 0..3 {
            outputs.push(runtime.block_on(decision.evaluate_with_opts(input(), options)));
        }
        outputs.extend(runtime.block_on(decision.evaluate_batch(&[input(), input()], options)));
        decision.compile();
        outputs.push(runtime.block_on(decision.evaluate_with_opts(input(), options)));
        for (at, output) in outputs.into_iter().enumerate() {
            let output = output.unwrap().result.to_value();
            assert_eq!(output["typed"], json!(true), "{zone} call {at} {output}");
            assert_eq!(
                output["shown"],
                json!("2025-01-01 00:00"),
                "{zone} call {at} {output}"
            );
        }
    }
}

#[test]
fn script_dates_agree_far_from_today() {
    let bodies = [
        "({ v: new Date(2100, 6, 1).getTimezoneOffset(), w: new Date(2100, 0, 1).getTimezoneOffset() })",
        "({ v: new Date(2250, 6, 1, 12).getTime(), w: new Date(2399, 11, 31, 23, 59).getHours() })",
        "({ v: new Date(-62183203200000).toString().split(' ')[3] })",
        "({ v: new Date(-62183203200000).toLocaleString().split(',')[0] })",
        "({ v: String(new Date(8.6e15)), w: new Date(-8.6e15).getFullYear() })",
        "({ v: new Date(['2024-01-01']).getTime(), w: new Date([5]).getTime() })",
        "({ v: new Date({ valueOf: () => 86400000 }).getTime(), w: new Date({ toString: () => '2024-03-01T00:00:00Z' }).getTime() })",
    ];
    for body in bodies {
        let source = format!("export const handler = async (input) => {body};");
        let graph = graph(
            json!({"id": "node", "name": "node", "type": "functionNode", "content": {"source": source}}),
        );
        let want = outcomes(EngineVersion::V1, Path::Repeated, &graph, &json!({})).remove(0);
        assert!(want.get("error").is_none(), "{body} {want}");
        for path in [Path::Repeated, Path::Compiled, Path::Batch] {
            for got in outcomes(EngineVersion::V2, path, &graph, &json!({})) {
                assert_eq!(got, want, "{body} {path:?}");
            }
        }
    }
    let source = "export const handler = async (input) => ({ v: new Date(2020n).getTime() });";
    let graph = graph(
        json!({"id": "node", "name": "node", "type": "functionNode", "content": {"source": source}}),
    );
    for version in [EngineVersion::V1, EngineVersion::V2] {
        let got = outcomes(version, Path::Repeated, &graph, &json!({})).remove(0);
        assert!(got.get("error").is_some(), "{version:?} {got}");
    }
}

#[test]
fn a_script_error_names_its_node_on_every_traced_batch_row() {
    let graph = graph(json!({
        "id": "node", "name": "node", "type": "functionNode",
        "content": {"source": "export const handler = async (input) => { throw new Error('boom'); }"}
    }));
    let content: DecisionContent = serde_json::from_value(graph).unwrap();
    let decision = DecisionEngine::default()
        .with_version(EngineVersion::V2)
        .create_decision(Arc::new(content))
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let inputs: Vec<Variable> = (1..=3).map(|x| Variable::from(json!({"x": x}))).collect();
    let shown = |error: Box<zen_engine::EvaluationError>| serde_json::to_value(&error).unwrap();
    let single = shown(
        runtime
            .block_on(decision.evaluate(inputs[0].clone()))
            .unwrap_err(),
    );
    assert!(single.to_string().contains("node:node:"), "{single}");
    for trace in [
        zen_engine::trace::TraceFormat::Legacy,
        zen_engine::trace::TraceFormat::V1,
    ] {
        let options = EvaluationOptions {
            trace,
            ..Default::default()
        };
        let rows = runtime.block_on(decision.evaluate_batch(&inputs, options));
        for (at, row) in rows.into_iter().enumerate() {
            let got = shown(row.unwrap_err());
            let message = |value: &Value| value["source"].to_string();
            assert!(
                got.to_string().contains("node:node:"),
                "{trace:?} row {at}: {got}"
            );
            assert_eq!(message(&got), message(&single), "{trace:?} row {at}");
        }
    }
}

#[test]
fn a_loop_entry_reading_earlier_entries_in_a_closure_keeps_every_item() {
    let graph = graph(json!({
        "id": "node", "name": "node", "type": "expressionNode",
        "content": {
            "inputField": "items",
            "executionMode": "loop",
            "expressions": [
                {"id": "a", "key": "s", "value": "k"},
                {"id": "b", "key": "o", "value": "some([1], $.s == 'a')"}
            ]
        }
    }));
    same_on_every_path(
        &graph,
        &json!({"items": [{"k": "a"}, {"k": "b"}]}),
        json!([{"s": "a", "o": true}, {"s": "b", "o": false}]),
    );
}

#[test]
fn tracing_keeps_a_table_field_that_reads_the_previous_cell() {
    let graph = graph(json!({
        "id": "node", "name": "node", "type": "decisionTableNode",
        "content": {
            "hitPolicy": "collect",
            "inputs": [{"id": "i0", "field": "p"}, {"id": "i1", "field": "$ ?? 0"}],
            "outputs": [{"id": "o0", "field": "o"}],
            "rules": [
                {"_id": "r0", "i1": "", "o0": "0"},
                {"_id": "r1", "i0": "> 0", "i1": "", "o0": "1"},
                {"_id": "r2", "i0": "> 0", "i1": "!= null", "o0": "$"}
            ]
        }
    }));
    let content: DecisionContent = serde_json::from_value(graph).unwrap();
    let content = Arc::new(content);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let want = json!([{"o": 0}, {"o": 1}, {"o": 2}]);
    for version in [EngineVersion::V1, EngineVersion::V2] {
        let decision = DecisionEngine::default()
            .with_version(version)
            .create_decision(content.clone())
            .unwrap();
        for trace in [
            zen_engine::trace::TraceFormat::Off,
            zen_engine::trace::TraceFormat::Legacy,
            zen_engine::trace::TraceFormat::V1,
        ] {
            let options = EvaluationOptions {
                trace,
                ..Default::default()
            };
            for call in 0..3 {
                let got = runtime
                    .block_on(decision.evaluate_with_opts(Variable::from(json!({"p": 2})), options))
                    .unwrap()
                    .result
                    .to_value();
                assert_eq!(got, want, "{version:?} {trace:?} call {call}");
            }
            let rows = vec![Variable::from(json!({"p": 2})); 2];
            for (at, row) in runtime
                .block_on(decision.evaluate_batch(&rows, options))
                .into_iter()
                .enumerate()
            {
                assert_eq!(
                    row.unwrap().result.to_value(),
                    want,
                    "{version:?} {trace:?} batch row {at}"
                );
            }
        }
    }
}

#[test]
fn a_batch_row_reads_node_values_beside_an_input_field_built_upstream() {
    let graph = chained(vec![
        json!({"id": "n3", "name": "n3", "type": "expressionNode", "content": {"expressions": [
            {"id": "a", "key": "obj.k", "value": "1"}
        ]}}),
        json!({"id": "n4", "name": "n4", "type": "expressionNode", "content": {
            "inputField": "obj",
            "expressions": [
                {"id": "b", "key": "out", "value": "type($nodes.n3)"},
                {"id": "c", "key": "k", "value": "$nodes.n3.obj.k"}
            ]
        }}),
    ]);
    batch_rows_match_single_rows(&graph, &[json!({}), json!({"x": 1})]);
}

#[test]
fn a_batch_row_reads_node_values_after_a_table_writing_the_root() {
    let graph = chained(vec![
        json!({"id": "n0", "name": "n0", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "outputPath": "$",
            "inputs": [],
            "outputs": [{"id": "o", "field": "y"}],
            "rules": [{"_id": "r0", "o": "1"}]
        }}),
        json!({"id": "n1", "name": "n1", "type": "expressionNode", "content": {"expressions": [
            {"id": "a", "key": "got", "value": "$nodes.in"},
            {"id": "b", "key": "prev", "value": "$nodes.n0"}
        ]}}),
    ]);
    batch_rows_match_single_rows(&graph, &[json!({"a": 1}), json!({"a": 2})]);
}

#[test]
fn a_batch_trace_keeps_the_references_of_a_described_rule() {
    let graph = graph(json!({
        "id": "node", "name": "node", "type": "decisionTableNode",
        "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "a"}, {"id": "i1", "field": "c"}],
            "outputs": [{"id": "o0", "field": "o"}],
            "rules": [{"_id": "r0", "_description": "x", "i0": "> 0", "i1": ">= 85", "o0": "1"}]
        }
    }));
    let content: DecisionContent = serde_json::from_value(graph).unwrap();
    let decision = DecisionEngine::default()
        .with_version(EngineVersion::V2)
        .create_decision(Arc::new(content))
        .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let request = Variable::from(json!({"a": 1, "c": 92.3}));
    let options = EvaluationOptions {
        trace: zen_engine::trace::TraceFormat::V1,
        ..Default::default()
    };
    let references = |response: zen_engine::DecisionGraphResponse| {
        let document = serde_json::to_value(&response).unwrap()["trace"].to_string();
        let restored = zen_engine::trace::restore(zen_engine::trace::RestoreArgs {
            trace: zen_engine::trace::Input::Json(&document),
            request: zen_engine::trace::Input::Variable(&request),
            documents: None,
        })
        .unwrap();
        serde_json::to_value(&restored).unwrap()["node"]["traceData"]["reference_map"].clone()
    };
    let single = references(
        runtime
            .block_on(decision.evaluate_with_opts(request.clone(), options))
            .unwrap(),
    );
    assert_eq!(single, json!({"a": 1, "c": 92.3}));
    let rows = vec![request.clone(), request.clone()];
    for (at, row) in runtime
        .block_on(decision.evaluate_batch(&rows, options))
        .into_iter()
        .enumerate()
    {
        assert_eq!(references(row.unwrap()), single, "batch row {at}");
    }
}

#[test]
fn a_batch_row_keeps_a_null_written_under_an_upstream_scalar() {
    let graph = chained(vec![
        json!({"id": "n0", "name": "n0", "type": "expressionNode", "content": {"expressions": [
            {"id": "a", "key": "res", "value": "1"}
        ]}}),
        json!({"id": "n1", "name": "n1", "type": "expressionNode", "content": {
            "passThrough": true,
            "expressions": [{"id": "b", "key": "res.k", "value": "null"}]
        }}),
    ]);
    batch_rows_match_single_rows(&graph, &[json!({}), json!({})]);
}

#[test]
fn a_batch_row_keeps_an_object_of_null_leaves() {
    let graph = chained(vec![
        json!({"id": "n0", "name": "n0", "type": "expressionNode", "content": {"expressions": [
            {"id": "a", "key": "q", "value": "1"}
        ]}}),
        json!({"id": "n1", "name": "n1", "type": "expressionNode", "content": {
            "passThrough": true,
            "expressions": [{"id": "b", "key": "obj.a", "value": "null"}]
        }}),
        json!({"id": "n2", "name": "n2", "type": "expressionNode", "content": {"expressions": [
            {"id": "c", "key": "r", "value": "obj"},
            {"id": "d", "key": "t", "value": "type(obj)"}
        ]}}),
    ]);
    batch_rows_match_single_rows(&graph, &[json!({}), json!({})]);
}

#[test]
fn a_batch_row_keeps_nested_nulls_through_duplicate_edges() {
    let graph = json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            {"id": "n4", "name": "n4", "type": "expressionNode", "content": {"expressions": [
                {"id": "a", "key": "v.w", "value": "null"}
            ]}},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "n4", "type": "edge"},
            {"id": "e2", "sourceId": "n4", "targetId": "out", "type": "edge"},
            {"id": "e3", "sourceId": "n4", "targetId": "out", "type": "edge"}
        ]
    });
    batch_rows_match_single_rows(&graph, &[json!({}), json!({})]);
}

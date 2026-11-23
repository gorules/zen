#[allow(dead_code)]
mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
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

fn expression(value: &str) -> Value {
    graph(
        json!({"id": "node", "name": "node", "type": "expressionNode", "content": {"expressions": [
            {"id": "x0", "key": "o", "value": value}
        ]}}),
    )
}

fn columns_match_version_one(graph: Value, rows: Vec<Value>) {
    let content: Arc<DecisionContent> = Arc::new(serde_json::from_value(graph).unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let old = DecisionEngine::default()
        .with_version(EngineVersion::V1)
        .create_decision(content.clone())
        .unwrap();
    let new = DecisionEngine::default()
        .with_version(EngineVersion::V2)
        .create_decision(content)
        .unwrap();
    let built = Built::lists(&rows).or_else(|| Built::new(&rows)).unwrap();
    let fields = built.fields();
    let structs = built.structs(&fields);
    let columns = built.columns_with(&structs);
    let output = runtime.block_on(new.evaluate_columns(&columns, EvaluationOptions::default()));
    for (at, row) in rows.iter().enumerate() {
        let want = runtime
            .block_on(old.evaluate(Variable::from(row.clone())))
            .map(|response| Built::normalized(response.result.to_value()));
        match (&want, &output.errors[at]) {
            (Err(_), Some(_)) => {}
            (Ok(want), None) => {
                assert_eq!(
                    &Built::normalized(output.row(at).to_value()),
                    want,
                    "row {at}"
                )
            }
            (want, got) => panic!("row {at}: version 1 {want:?}, columns {got:?}"),
        }
    }
}

#[test]
fn a_choice_between_two_equal_arms_fills_every_row() {
    columns_match_version_one(
        expression("flag ? upper(s) : upper(s)"),
        vec![
            json!({"s": "q", "flag": false}),
            json!({"s": "r", "flag": true}),
        ],
    );
    columns_match_version_one(
        expression("flag ? round(ts / 7, 2) : round(ts / 7, 2)"),
        vec![
            json!({"ts": 5, "flag": false}),
            json!({"ts": 9, "flag": true}),
        ],
    );
}

#[test]
fn membership_inside_a_closure_reads_the_whole_list() {
    columns_match_version_one(
        expression("filter(nums, # * 2 in nums)"),
        vec![json!({"nums": [1, 2, 4]}), json!({"nums": [3, 6]})],
    );
    columns_match_version_one(
        expression("filter(strs, # + 'x' in strs)"),
        vec![
            json!({"strs": ["a", "ax", "b"]}),
            json!({"strs": ["b", "bx"]}),
        ],
    );
}

#[test]
fn a_failed_row_with_a_tiny_decimal_leaves_the_others_intact() {
    columns_match_version_one(
        expression("(1.50 * ((a + b) ?? b)) in (1.50..4294967295]"),
        vec![
            json!({"a": null, "b": 0.0000000000000000000000000001}),
            json!({"a": 1, "b": 2}),
        ],
    );
    columns_match_version_one(
        expression("sum(map(filter(objs, #.on), #.x)) * a > 1"),
        vec![
            json!({"a": 0.0000000000000000000000000001, "objs": [{"x": 0.0000000000000000000000000001, "on": true}, {"x": 2}]}),
            json!({"a": 1, "objs": [{"x": 2, "on": true}]}),
        ],
    );
}

#[test]
fn comparing_different_types_keeps_operand_errors() {
    columns_match_version_one(
        expression("true != n + 1"),
        vec![json!({"n": null}), json!({"n": 1})],
    );
}

#[test]
fn membership_keeps_the_error_of_the_tested_value() {
    columns_match_version_one(
        expression("(n + 1) in nums"),
        vec![
            json!({"nums": [1, 2], "n": 1}),
            json!({"nums": [], "n": null}),
            json!({"nums": [3], "n": 2}),
        ],
    );
}

#[test]
fn a_date_interval_evaluates_both_bounds() {
    columns_match_version_one(
        expression("d(ds) in [d('2025-01-01')..d(ds).add('1D'))"),
        vec![json!({"ds": "2024-01-15"}), json!({"ds": "2024-01-16"})],
    );
    columns_match_version_one(
        expression("d(ds) in [d('2024-01-01')..d(ds).add(1, 'd'))"),
        vec![
            json!({"ds": "2024-01-15"}),
            json!({"ds": "2023-06-01"}),
            json!({"ds": "2025-01-01"}),
        ],
    );
}

#[test]
fn a_table_cell_fails_with_its_input_field() {
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "lower(s)"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": "1 < 2", "o0": "'r0'"},
                {"_id": "r1", "i0": "", "o0": "'r1'"}
            ]
        }}),
    );
    columns_match_version_one(table, vec![json!({"s": null}), json!({"s": "A"})]);
}

#[test]
fn policy_columns_keep_the_error_of_a_tested_value() {
    let policy: zen_engine::policy::PolicyDocument = serde_json::from_value(json!({
        "blocks": [
            {"id": "dm", "type": "dataModel", "props": {"data": {"name": "c", "properties": [
                {"id": "p1", "name": "n", "type": "number", "array": false, "optional": true},
                {"id": "p2", "name": "s", "type": "string", "array": false, "optional": true},
                {"id": "p3", "name": "list", "type": "string", "array": true, "optional": true}
            ]}}, "children": []},
            {"id": "e1", "type": "expression", "props": {"data": {
                "key": "c.a", "value": "string(c.n + 1) in (c.list ?? [])"
            }}},
            {"id": "e2", "type": "expression", "props": {"data": {
                "key": "c.b", "value": "string(upper(c.s)) in (c.list ?? [])"
            }}}
        ]
    }))
    .unwrap();
    let rows = vec![
        json!({"c": {"n": 1, "s": "2", "list": ["2"]}}),
        json!({"c": {"n": null, "s": "a", "list": ["a"]}}),
        json!({"c": {"n": null, "s": null, "list": null}}),
        json!({"c": {"n": null, "s": null, "list": []}}),
    ];
    let path: Arc<str> = Arc::from("policy");
    let options = EvaluationOptions::default();
    let workspace = |version| {
        let mut workspace = zen_engine::Workspace::new().with_version(version);
        workspace.set_policy("policy", policy.clone());
        workspace
    };
    let old = workspace(EngineVersion::V1);
    let new = workspace(EngineVersion::V2);
    let built = Built::lists(&rows).unwrap();
    let fields = built.fields();
    let structs = built.structs(&fields);
    let columns = built.columns_with(&structs);
    let output = new
        .evaluate_columns_pinned(&path, &[], &columns, &options)
        .unwrap();
    for (at, row) in rows.iter().enumerate() {
        let request = zen_engine::policy::EvaluateRequest {
            policy_path: path.clone(),
            input: Variable::from(row.clone()),
            goals: Vec::new(),
            trace: zen_engine::trace::TraceFormat::Off,
        };
        let want = old
            .evaluate_pinned(&request, &options)
            .map(|result| result.output.to_value()["c"].clone());
        match (&want, &output.errors[at]) {
            (Err(_), Some(_)) => {}
            (Ok(want), None) => {
                let got = output.row(at).to_value()["c"].clone();
                assert_eq!(got["a"], want["a"], "row {at}");
                assert_eq!(got["b"], want["b"], "row {at}");
            }
            (want, got) => panic!("row {at}: version 1 {want:?}, columns {got:?}"),
        }
    }
}

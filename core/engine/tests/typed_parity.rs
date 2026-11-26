#[allow(dead_code)]
mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use zen_engine::diagnostics::Paths;
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

fn columns_match_version_one(graph: Value, rows: Vec<Value>) -> Paths {
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
    let wants: Vec<Result<Value, String>> = rows
        .iter()
        .map(|row| {
            runtime
                .block_on(old.evaluate(Variable::from(row.clone())))
                .map(|response| Built::normalized(response.result.to_value()))
                .map_err(|e| e.to_string())
        })
        .collect();
    let repeated: Vec<Value> = rows
        .iter()
        .cycle()
        .take((rows.len() * 8).max(32))
        .cloned()
        .collect();
    let paths = columns_match(&runtime, &new, &rows, &wants);
    columns_match(&runtime, &new, &repeated, &wants);
    paths
}

fn columns_match(
    runtime: &tokio::runtime::Runtime,
    new: &zen_engine::Decision,
    rows: &[Value],
    wants: &[Result<Value, String>],
) -> Paths {
    let built = Built::lists(rows).or_else(|| Built::new(rows)).unwrap();
    let fields = built.fields();
    let structs = built.structs(&fields);
    let columns = built.columns_with(&structs);
    let output = runtime.block_on(new.evaluate_columns(&columns, EvaluationOptions::default()));
    for at in 0..rows.len() {
        let want = &wants[at % wants.len()];
        match (want, &output.errors[at]) {
            (Err(_), Some(_)) => {}
            (Ok(want), None) => {
                assert_eq!(
                    &Built::normalized(output.row(at).to_value()),
                    want,
                    "row {at} of {}",
                    rows.len()
                )
            }
            (want, got) => panic!(
                "row {at} of {}: version 1 {want:?}, columns {got:?}",
                rows.len()
            ),
        }
    }
    output.paths()
}

fn typed_like_version_one(graph: Value, rows: Vec<Value>) {
    let label = graph.to_string();
    let paths = columns_match_version_one(graph, rows);
    assert!(
        paths.typed > 0 && paths.laned == 0,
        "{label}: expected typed, got {paths:?}"
    );
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

fn numbers() -> Vec<Value> {
    vec![
        json!({"n": 1, "nums": [1, 2]}),
        json!({"n": null, "nums": [1, 2]}),
        json!({"n": 2, "nums": []}),
        json!({"n": null, "nums": []}),
        json!({"n": 0, "nums": null}),
        json!({"n": null, "nums": null}),
        json!({"n": 5, "nums": [3, 4]}),
        json!({"n": null, "nums": [0]}),
    ]
}

fn texts() -> Vec<Value> {
    vec![
        json!({"s": "a", "strs": ["A", "b"]}),
        json!({"s": null, "strs": ["A"]}),
        json!({"s": "b", "strs": []}),
        json!({"s": null, "strs": []}),
        json!({"s": "c", "strs": null}),
        json!({"s": null, "strs": null}),
        json!({"s": "z", "strs": ["Q"]}),
    ]
}

fn dates() -> Vec<Value> {
    vec![
        json!({"ds": "2024-01-15", "days": ["2024-01-15", "2024-02-01"]}),
        json!({"ds": null, "days": ["2024-01-15"]}),
        json!({"ds": "2024-03-01", "days": []}),
        json!({"ds": null, "days": []}),
        json!({"ds": "2024-03-01", "days": null}),
        json!({"ds": null, "days": null}),
        json!({"ds": "2024-05-05", "days": ["2024-01-15"]}),
    ]
}

#[test]
fn membership_of_a_computed_value_runs_typed() {
    for source in [
        "(n + 1) in nums",
        "(n + 1) not in nums",
        "contains(nums, n + 1)",
        "contains([], n + 1)",
        "contains([3], n * 3)",
        "not contains(nums, n * 2)",
        "abs(n) in nums",
        "(n + 1) in nums or n == 1",
        "n == 1 and (n - 1) not in nums",
    ] {
        typed_like_version_one(expression(source), numbers());
    }
    for source in [
        "upper(s) in strs",
        "upper(s) not in strs",
        "contains(strs, upper(s))",
        "(s + 'x') in strs",
    ] {
        typed_like_version_one(expression(source), texts());
    }
    for source in ["date(ds) in days", "date(ds) not in days"] {
        typed_like_version_one(expression(source), dates());
    }
}

#[test]
fn comparing_a_computed_value_of_another_type_runs_typed() {
    let rows = vec![
        json!({"n": null, "a": 1, "b": 2, "c": 3, "ds": "2024-01-01", "flag": true}),
        json!({"n": 1, "a": null, "b": 2, "c": 3, "ds": null, "flag": null}),
        json!({"n": 2, "a": 3, "b": null, "c": null, "ds": "2024-02-01", "flag": false}),
        json!({"n": null, "a": null, "b": null, "c": null, "ds": null, "flag": null}),
        json!({"n": 0, "a": 1, "b": 1, "c": 1, "ds": "2024-01-01", "flag": true}),
    ];
    for source in [
        "true != n + 1",
        "true == n + 1",
        "'a' == n + 1",
        "'a' != n + 1",
        "n + 1 == 'a'",
        "d('2024-01-15') == n + 1",
        "d('2024-01-15') != n + 1",
        "date(ds) == n + 1",
        "date(ds) != 1",
        "flag == n + 1",
        "a <= b != c",
        "a <= b == c",
        "(a + b) == 'x'",
        "upper(ds) == 1",
    ] {
        typed_like_version_one(expression(source), rows.clone());
    }
}

fn table(policy: &str, field: &str, cells: [&str; 2]) -> Value {
    graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": policy,
            "inputs": [{"id": "i0", "field": field}, {"id": "i1", "field": "k"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": cells[0], "i1": "", "o0": "'r0'"},
                {"_id": "r1", "i0": cells[1], "i1": "> 0", "o0": "'r1'"},
                {"_id": "r2", "i0": "", "i1": "", "o0": "'r2'"}
            ]
        }}),
    )
}

#[test]
fn a_table_cell_on_a_computed_field_runs_typed() {
    let rows = vec![
        json!({"s": null, "n": null, "a": null, "k": 1}),
        json!({"s": "A", "n": -2, "a": 2, "k": 1}),
        json!({"s": "b", "n": 3, "a": 0, "k": 0}),
        json!({"s": null, "n": 1, "a": null, "k": 2}),
        json!({"s": "Q", "n": null, "a": 5, "k": null}),
    ];
    for policy in ["first", "collect"] {
        for (field, cells) in [
            ("lower(s)", ["1 < 2", "k > 1"]),
            ("lower(s)", ["'a' == $", "true"]),
            ("lower(s)", ["k > 1 or $ == 'q'", "\"q\""]),
            ("abs(n)", ["k > 1", "> 1"]),
            ("abs(n)", ["[1, 2]", "not(k > 0)"]),
            ("a + 1", ["true", "$ > 2"]),
            ("a + 1", ["k == 1", "k == 2 and $ > 0"]),
        ] {
            typed_like_version_one(table(policy, field, cells), rows.clone());
        }
    }
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
            }}},
            {"id": "e3", "type": "expression", "props": {"data": {
                "key": "c.d", "value": "'x' != c.n * 2"
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
    let paths = output.paths();
    assert!(paths.typed > 0 && paths.laned == 0, "{paths:?}");
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
                assert_eq!(got["d"], want["d"], "row {at}");
            }
            (want, got) => panic!("row {at}: version 1 {want:?}, columns {got:?}"),
        }
    }
}

#[test]
fn column_traces_come_in_the_requested_format() {
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "a"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": "> 1", "o0": "'big'"},
                {"_id": "r1", "i0": "", "o0": "'small'"}
            ]
        }}),
    );
    let content: Arc<DecisionContent> = Arc::new(serde_json::from_value(table).unwrap());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let decision = DecisionEngine::default()
        .with_version(EngineVersion::V2)
        .create_decision(content)
        .unwrap();
    let rows = vec![json!({"a": 1}), json!({"a": 2})];
    let built = Built::new(&rows).unwrap();
    let columns = built.columns();
    let timeless = |mut value: Value| {
        fn strip(value: &mut Value) {
            match value {
                Value::Object(map) => {
                    map.remove("performance");
                    map.values_mut().for_each(strip);
                }
                Value::Array(items) => items.iter_mut().for_each(strip),
                _ => {}
            }
        }
        strip(&mut value);
        value
    };
    for format in [
        zen_engine::trace::TraceFormat::Legacy,
        zen_engine::trace::TraceFormat::V1,
    ] {
        let options = EvaluationOptions {
            trace: format,
            ..Default::default()
        };
        let output = runtime.block_on(decision.evaluate_columns(&columns, options));
        let mut all = String::new();
        let ends = output.traces().unwrap().write_all(&mut all);
        for (at, row) in rows.iter().enumerate() {
            let single = runtime
                .block_on(decision.evaluate_with_opts(Variable::from(row.clone()), options))
                .unwrap();
            let want = serde_json::to_value(&single).unwrap()["trace"].clone();
            let got: Value = serde_json::from_str(&output.trace_json(at).unwrap()).unwrap();
            let start = at.checked_sub(1).map_or(0, |before| ends[before] + 1);
            let line: Value = serde_json::from_str(&all[start..ends[at]]).unwrap();
            let legacy = format == zen_engine::trace::TraceFormat::Legacy;
            assert_eq!(got.get("version").is_none(), legacy, "{format:?} row {at}");
            assert_eq!(timeless(line), timeless(got.clone()), "{format:?} row {at}");
            if legacy {
                assert_eq!(timeless(got), timeless(want), "{format:?} row {at}");
            }
        }
    }
}

fn chain(nodes: Vec<Value>) -> Value {
    let mut all = vec![json!({"id": "in", "name": "in", "type": "inputNode"})];
    all.extend(nodes);
    all.push(json!({"id": "out", "name": "out", "type": "outputNode"}));
    let ids: Vec<String> = all
        .iter()
        .map(|node| node["id"].as_str().unwrap().to_string())
        .collect();
    let edges: Vec<Value> = ids
        .windows(2)
        .map(|pair| json!({"id": format!("{}-{}", pair[0], pair[1]), "sourceId": pair[0], "targetId": pair[1], "type": "edge"}))
        .collect();
    json!({"nodes": all, "edges": edges})
}

#[test]
fn columns_merge_a_value_written_back_like_version_1() {
    let expression = |id: &str, pass: bool, key: &str, value: &str| {
        json!({"id": id, "name": id, "type": "expressionNode", "content": {
            "passThrough": pass,
            "expressions": [{"id": format!("{id}e"), "key": key, "value": value}]
        }})
    };
    let rows = vec![json!({"n": 1}), json!({"n": 2, "a": 5})];
    for graph in [
        chain(vec![
            expression("n1", false, "x", "{k: a, n: n}"),
            expression("n2", true, "x", "x"),
        ]),
        chain(vec![
            expression("n1", false, "x", "{k: a, n: n}"),
            expression("n2", true, "x", "$nodes.n1.x"),
        ]),
        chain(vec![
            expression("n1", false, "a", "{b: {res: null, n: n}}"),
            expression("n2", true, "a", "$nodes.n1.a"),
        ]),
    ] {
        columns_match_version_one(graph, rows.clone());
    }
}

fn collect_table(
    output_path: Option<&str>,
    inputs: Value,
    outputs: Value,
    rules: Vec<Value>,
) -> Value {
    let mut content = json!({
        "hitPolicy": "collect",
        "passThrough": output_path.is_some(),
        "inputs": inputs,
        "outputs": outputs,
        "rules": rules
    });
    if let Some(path) = output_path {
        content["outputPath"] = json!(path);
    }
    graph(json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": content}))
}

fn collect_rows() -> Vec<Value> {
    vec![
        json!({"tier": "gold", "n": 5, "s": "a", "tags": ["x", "y"], "flag": true}),
        json!({"tier": "silver", "n": null, "s": null, "tags": [], "flag": false}),
        json!({"tier": null, "n": 12, "s": "B", "tags": ["y"], "flag": null}),
        json!({"tier": "bronze", "n": -1, "s": "c", "tags": ["z"], "flag": true}),
        json!({"tier": "gold", "n": 0, "s": "", "tags": ["x"], "flag": false}),
        json!({"tier": "other", "n": 100, "s": "zz", "tags": [], "flag": true}),
    ]
}

#[test]
fn a_collect_table_runs_typed() {
    let inputs = json!([{"id": "i0", "field": "tier"}, {"id": "i1", "field": "n"}, {"id": "i2"}]);
    let outputs = json!([{"id": "o0", "field": "points"}, {"id": "o1", "field": "label"}]);
    let rules = vec![
        json!({"_id": "r0", "i0": "'gold'", "i1": "", "i2": "", "o0": "10", "o1": "'gold'"}),
        json!({"_id": "r1", "i0": "", "i1": "> 3", "i2": "", "o0": "5", "o1": ""}),
        json!({"_id": "r2", "i0": "'silver', 'bronze'", "i1": "", "i2": "flag == true", "o0": "", "o1": ""}),
        json!({"_id": "r3", "i0": "", "i1": "[0..10]", "i2": "upper(s) == 'A'", "o0": "1", "o1": "'small'"}),
        json!({"_id": "r4", "i0": "", "i1": "", "i2": "n * 2 > 20", "o0": "2", "o1": "'doubled'"}),
    ];
    for path in [None, Some("hits"), Some("result.hits")] {
        typed_like_version_one(
            collect_table(path, inputs.clone(), outputs.clone(), rules.clone()),
            collect_rows(),
        );
    }
}

#[test]
fn a_collect_table_with_nested_and_list_outputs_runs_typed() {
    let inputs = json!([{"id": "i0", "field": "tags"}, {"id": "i1"}]);
    let outputs =
        json!([{"id": "o0", "field": "carrier.name"}, {"id": "o1", "field": "carrier.codes"}]);
    let rules = vec![
        json!({"_id": "r0", "i0": "contains($, 'x')", "i1": "", "o0": "'ex'", "o1": "['a', 'b']"}),
        json!({"_id": "r1", "i0": "contains($, 'y')", "i1": "n > 1", "o0": "'why'", "o1": "[]"}),
        json!({"_id": "r2", "i0": "", "i1": "len(s) > 1", "o0": "'long'", "o1": "[1, 2]"}),
        json!({"_id": "r3", "i0": "", "i1": "tier == null", "o0": "", "o1": ""}),
    ];
    typed_like_version_one(
        collect_table(Some("carriers"), inputs, outputs, rules),
        collect_rows(),
    );
}

#[test]
fn a_collect_table_feeds_a_later_node() {
    let content = json!({
        "hitPolicy": "collect",
        "passThrough": true,
        "outputPath": "found",
        "inputs": [{"id": "i0", "field": "n"}],
        "outputs": [{"id": "o0", "field": "v"}],
        "rules": [
            {"_id": "r0", "i0": "> 0", "o0": "1"},
            {"_id": "r1", "i0": "> 10", "o0": "2"},
            {"_id": "r2", "i0": "< 0", "o0": "3"}
        ]
    });
    let graph = chain(vec![
        json!({"id": "t", "name": "t", "type": "decisionTableNode", "content": content}),
        json!({"id": "e", "name": "e", "type": "expressionNode", "content": {
            "passThrough": true,
            "expressions": [
                {"id": "e0", "key": "count", "value": "len(found)"},
                {"id": "e1", "key": "total", "value": "sum(map(found, #.v))"}
            ]
        }}),
    ]);
    let paths = columns_match_version_one(graph, collect_rows());
    assert!(paths.typed > 0, "{paths:?}");
}

#[test]
fn a_first_hit_table_with_collect_columns_runs_typed() {
    let inputs = json!([{"id": "i0", "field": "tier"}, {"id": "i1", "field": "n"}, {"id": "i2"}]);
    let outputs = json!([
        {"id": "o0", "field": "label"},
        {"id": "o1", "field": "reasons[]"},
        {"id": "o2", "field": "scores[]"}
    ]);
    let rules = vec![
        json!({"_id": "r0", "i0": "'gold'", "i1": "", "i2": "", "o0": "'gold'", "o1": "'tier'", "o2": "10"}),
        json!({"_id": "r1", "i0": "", "i1": "> 3", "i2": "", "o0": "'big'", "o1": "'size'", "o2": ""}),
        json!({"_id": "r2", "i0": "", "i1": "", "i2": "upper(s) == 'A'", "o0": "", "o1": "'letter'", "o2": "1"}),
        json!({"_id": "r3", "i0": "'silver', 'bronze'", "i1": "", "i2": "n * 2 > 0", "o0": "'metal'", "o1": "", "o2": ""}),
        json!({"_id": "r4", "i0": "'silver', 'bronze', 'other'", "i1": "", "i2": "", "o0": "", "o1": "'rest'", "o2": ""}),
    ];
    for path in [None, Some("found")] {
        let mut content = json!({
            "hitPolicy": "first",
            "passThrough": path.is_some(),
            "inputs": inputs.clone(),
            "outputs": outputs.clone(),
            "rules": rules.clone()
        });
        if let Some(path) = path {
            content["outputPath"] = json!(path);
        }
        typed_like_version_one(
            graph(
                json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": content}),
            ),
            collect_rows(),
        );
    }
}

fn conversions() -> Vec<Value> {
    vec![
        json!({"s": "12", "n": 1, "b": true, "ds": "2024-01-15"}),
        json!({"s": " 3.50 ", "n": null, "b": false, "ds": null}),
        json!({"s": "1e3", "n": -2.5, "b": null, "ds": "2024-02-01"}),
        json!({"s": "abc", "n": 0, "b": true, "ds": "2024-03-01"}),
        json!({"s": "", "n": 7, "b": false, "ds": "2024-04-01"}),
        json!({"s": null, "n": 3, "b": true, "ds": "2024-05-01"}),
        json!({"s": "-0.25", "n": 100, "b": false, "ds": "2024-06-01"}),
        json!({"s": "1 000", "n": 4, "b": true, "ds": "2024-07-01"}),
        json!({"s": "0x10", "n": 5, "b": true, "ds": "2024-08-01"}),
    ]
}

#[test]
fn number_runs_typed() {
    for source in [
        "number(s)",
        "number(s) + 1",
        "number(s) * n",
        "number(n)",
        "number(n) * 2",
        "number(b)",
        "number(b) + number(n)",
        "number(upper(s))",
        "number(string(n))",
        "number(date(ds))",
        "n + number(null)",
        "number('42') + n",
        "number(s) > 10 ? 'big' : 'small'",
    ] {
        typed_like_version_one(expression(source), conversions());
    }
}

#[test]
fn type_runs_typed() {
    for source in [
        "type(s)",
        "type(n)",
        "type(b)",
        "type(n + 1)",
        "type(upper(s))",
        "type(date(ds))",
        "type(null) + upper(s)",
        "type(number(s))",
        "type(n) != 'number'",
        "type(s) == 'string' and n > 0",
    ] {
        typed_like_version_one(expression(source), conversions());
    }
}

#[test]
fn number_in_a_table_cell_runs_typed() {
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "s"}, {"id": "i1"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": "number($) > 10", "i1": "", "o0": "'big'"},
                {"_id": "r1", "i0": "", "i1": "type(n) == 'null'", "o0": "'none'"},
                {"_id": "r2", "i0": "number($) > 0", "i1": "", "o0": "'small'"},
                {"_id": "r3", "i0": "", "i1": "", "o0": "'rest'"}
            ]
        }}),
    );
    typed_like_version_one(table, conversions());
}

#[test]
fn a_computed_table_output_read_by_a_later_node_runs_typed() {
    let table = json!({"id": "t", "name": "t", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "passThrough": true,
        "inputs": [{"id": "i0", "field": "tier"}],
        "outputs": [{"id": "o0", "field": "result.score"}, {"id": "o1", "field": "result.ok"}],
        "rules": [
            {"_id": "r0", "i0": "'gold'", "o0": "n * 2", "o1": "n > 3"},
            {"_id": "r1", "i0": "'silver'", "o0": "n + 1", "o1": ""},
            {"_id": "r2", "i0": "'bronze'", "o0": "", "o1": "true"}
        ]
    }});
    let reader = json!({"id": "e", "name": "e", "type": "expressionNode", "content": {
        "passThrough": false,
        "expressions": [
            {"id": "e0", "key": "score", "value": "result.score + 1"},
            {"id": "e1", "key": "flag", "value": "result.ok == true"},
            {"id": "e2", "key": "label", "value": "result.score > 5 ? 'high' : 'low'"}
        ]
    }});
    let paths = columns_match_version_one(chain(vec![table, reader]), collect_rows());
    assert!(paths.typed > 0, "{paths:?}");
    let rows: Vec<Value> = collect_rows()
        .into_iter()
        .filter(|row| row["tier"] == "gold" && row["n"].is_number())
        .collect();
    let table = json!({"id": "t", "name": "t", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "passThrough": true,
        "inputs": [{"id": "i0", "field": "tier"}],
        "outputs": [{"id": "o0", "field": "result.score"}],
        "rules": [{"_id": "r0", "i0": "'gold'", "o0": "n * 2"}]
    }});
    let reader = json!({"id": "e", "name": "e", "type": "expressionNode", "content": {
        "expressions": [{"id": "e0", "key": "score", "value": "result.score + 1"}]
    }});
    typed_like_version_one(chain(vec![table.clone(), reader.clone()]), rows.clone());
    let switched = json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            table,
            {"id": "s", "name": "s", "type": "switchNode", "content": {"hitPolicy": "first", "statements": [
                {"id": "s0", "condition": "result.score > 4"},
                {"id": "s1", "condition": ""}
            ]}},
            reader,
            {"id": "f", "name": "f", "type": "expressionNode", "content": {
                "expressions": [{"id": "f0", "key": "score", "value": "result.score - 1"}]
            }},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "a", "sourceId": "in", "targetId": "t", "type": "edge"},
            {"id": "b", "sourceId": "t", "targetId": "s", "type": "edge"},
            {"id": "c", "sourceId": "s", "sourceHandle": "s0", "targetId": "e", "type": "edge"},
            {"id": "d", "sourceId": "s", "sourceHandle": "s1", "targetId": "f", "type": "edge"},
            {"id": "g", "sourceId": "e", "targetId": "out", "type": "edge"},
            {"id": "h", "sourceId": "f", "targetId": "out", "type": "edge"}
        ]
    });
    typed_like_version_one(switched, rows);
}

#[test]
fn a_table_cell_or_output_that_always_fails_runs_typed() {
    let table = |inputs: Value, outputs: Value, rules: Vec<Value>| {
        graph(
            json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "inputs": inputs,
                "outputs": outputs,
                "rules": rules
            }}),
        )
    };
    typed_like_version_one(
        table(
            json!([{"id": "i0"}, {"id": "i1", "field": "flag"}]),
            json!([{"id": "o0", "field": "out"}]),
            vec![
                json!({"_id": "r0", "i0": "flag > 2", "i1": "", "o0": "'never'"}),
                json!({"_id": "r1", "i0": "", "i1": ">= 1", "o0": "'never either'"}),
                json!({"_id": "r2", "i0": "tier > n", "i1": "", "o0": "'text'"}),
                json!({"_id": "r3", "i0": "n > 3", "i1": "", "o0": "'big'"}),
                json!({"_id": "r4", "i0": "", "i1": "", "o0": "'rest'"}),
            ],
        ),
        collect_rows(),
    );
    typed_like_version_one(
        table(
            json!([{"id": "i0", "field": "tier"}]),
            json!([{"id": "o0", "field": "value"}, {"id": "o1", "field": "kind"}]),
            vec![
                json!({"_id": "r0", "i0": "'gold'", "o0": "missing * 0.5", "o1": "'gold'"}),
                json!({"_id": "r1", "i0": "'silver'", "o0": "n * 2", "o1": "'silver'"}),
                json!({"_id": "r2", "i0": "'gold', 'bronze'", "o0": "n - missing.value", "o1": "'metal'"}),
                json!({"_id": "r3", "i0": "", "o0": "n", "o1": "'rest'"}),
            ],
        ),
        collect_rows(),
    );
}

fn object_list_rows() -> Vec<Value> {
    vec![
        json!({"items": [{"kind": "a", "v": 5}, {"kind": "b", "v": 1}], "limit": 2}),
        json!({"items": [], "limit": 1}),
        json!({"items": [{"kind": "b", "v": null}], "limit": 0}),
        json!({"items": [{"kind": null, "v": 9}, {"kind": "a", "v": 2}, {"kind": "c", "v": 3}], "limit": 3}),
        json!({"items": [{"kind": "c", "v": 0}], "limit": null}),
    ]
}

#[test]
fn an_object_list_written_by_an_earlier_node_runs_typed() {
    let reader = json!({"id": "r", "name": "r", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "passThrough": true,
        "inputs": [{"id": "i0"}],
        "outputs": [{"id": "o0", "field": "level"}],
        "rules": [
            {"_id": "r0", "i0": "some(items, #.kind == 'a' and #.v > limit)", "o0": "'urgent'"},
            {"_id": "r1", "i0": "some(items, #.kind == 'b')", "o0": "'watch'"},
            {"_id": "r2", "i0": "", "o0": "'routine'"}
        ]
    }});
    typed_like_version_one(chain(vec![reader.clone()]), object_list_rows());
    let looped = json!({"id": "t", "name": "t", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "passThrough": true,
        "executionMode": "loop",
        "inputField": "items",
        "outputPath": "items",
        "inputs": [{"id": "i0", "field": "kind"}, {"id": "i1", "field": "v"}],
        "outputs": [{"id": "o0", "field": "kind"}, {"id": "o1", "field": "v"}],
        "rules": [
            {"_id": "r0", "i0": "'a'", "i1": "> 3", "o0": "'a'", "o1": "v * 2"},
            {"_id": "r1", "i0": "", "i1": "> 1", "o0": "'b'", "o1": "v"},
            {"_id": "r2", "i0": "", "i1": "", "o0": "'c'", "o1": "0"}
        ]
    }});
    let paths = columns_match_version_one(chain(vec![looped, reader]), object_list_rows());
    assert!(paths.typed > 0 && paths.laned == 0, "{paths:?}");
    let mapped = json!({"id": "m", "name": "m", "type": "expressionNode", "content": {
        "passThrough": true,
        "expressions": [{"id": "m0", "key": "objs", "value": "map(items, {k: #.kind, w: #.v})"}]
    }});
    let reader = json!({"id": "e", "name": "e", "type": "expressionNode", "content": {
        "expressions": [
            {"id": "e0", "key": "count", "value": "count(objs, #.k == 'a')"},
            {"id": "e1", "key": "any", "value": "some(objs, #.w > 2)"}
        ]
    }});
    let paths = columns_match_version_one(chain(vec![mapped, reader]), object_list_rows());
    assert!(paths.typed > 0, "{paths:?}");
}

#[test]
fn a_table_cell_without_a_field_that_does_not_parse_runs_typed() {
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "tier"}, {"id": "i1"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": "'gold'", "i1": "\"RU\", \"NK\"", "o0": "'list'"},
                {"_id": "r1", "i0": "", "i1": "> 5", "o0": "'unary'"},
                {"_id": "r2", "i0": "'silver'", "i1": "n > 3 or $ == null", "o0": "'dollar'"},
                {"_id": "r3", "i0": "", "i1": "n > 3", "o0": "'big'"},
                {"_id": "r4", "i0": "", "i1": "", "o0": "'rest'"}
            ]
        }}),
    );
    columns_match_version_one(table, collect_rows());
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "tier"}, {"id": "i1"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": "'gold'", "i1": "\"RU\", \"NK\"", "o0": "'list'"},
                {"_id": "r1", "i0": "", "i1": "> 5", "o0": "'unary'"},
                {"_id": "r2", "i0": "", "i1": "n > 3", "o0": "'big'"},
                {"_id": "r3", "i0": "", "i1": "", "o0": "'rest'"}
            ]
        }}),
    );
    typed_like_version_one(table, collect_rows());
}

#[test]
fn a_table_whose_inputs_are_always_missing_runs_typed() {
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "missing.score"}],
            "outputs": [{"id": "o0", "field": "grade"}, {"id": "o1", "field": "echo"}],
            "rules": [
                {"_id": "r0", "i0": ">= 700", "o0": "'good'", "o1": "n"},
                {"_id": "r1", "i0": "", "o0": "'poor'", "o1": "n + 1"}
            ]
        }}),
    );
    typed_like_version_one(table, collect_rows());
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0"}],
            "outputs": [{"id": "o0", "field": "flag"}],
            "rules": [
                {"_id": "r0", "i0": "\"RU\", \"NK\"", "o0": "'red'"},
                {"_id": "r1", "i0": "", "o0": "'green'"}
            ]
        }}),
    );
    let reader = json!({"id": "e", "name": "e", "type": "expressionNode", "content": {
        "passThrough": true,
        "expressions": [{"id": "e0", "key": "both", "value": "flag + ':' + tier"}]
    }});
    columns_match_version_one(
        chain(vec![table["nodes"][1].clone(), reader]),
        collect_rows(),
    );
}

#[test]
fn a_missing_text_equals_a_missing_date() {
    let rows = vec![
        json!({"s": null, "ds": "2024-01-15", "n": 1}),
        json!({"s": "2024-01-15", "ds": "2024-01-15", "n": 5}),
        json!({"s": null, "ds": null, "n": 7}),
        json!({"s": "x", "ds": "2024-02-01", "n": 2}),
        json!({"s": "2024-02-01", "ds": "2024-02-01", "n": 9}),
    ];
    for source in [
        "s == (n > 3 ? date(ds) : null)",
        "s != (n > 3 ? date(ds) : null)",
        "(n > 3 ? date(ds) : null) == s",
    ] {
        typed_like_version_one(expression(source), rows.clone());
    }
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "s"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": "(n > 3 ? date(ds) : null)", "o0": "'same'"},
                {"_id": "rz", "i0": "(missing == false ? d(n) : null)", "o0": "'none'"},
                {"_id": "r1", "i0": "", "o0": "'other'"}
            ]
        }}),
    );
    typed_like_version_one(table, rows);
}

fn entries(id: &str, pass: bool, pairs: &[(&str, &str)]) -> Value {
    let expressions: Vec<Value> = pairs
        .iter()
        .enumerate()
        .map(|(at, (key, value))| json!({"id": format!("{id}{at}"), "key": key, "value": value}))
        .collect();
    json!({"id": id, "name": id, "type": "expressionNode", "content": {
        "passThrough": pass,
        "expressions": expressions
    }})
}

fn columns_keep_version_one(graph: Value, rows: Vec<Value>, kept: &[&str]) -> Paths {
    let paths = columns_match_version_one(graph.clone(), rows.clone());
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
    let contexts: Vec<Variable> = rows.iter().cloned().map(Variable::from).collect();
    let batch = runtime.block_on(new.evaluate_batch(&contexts, EvaluationOptions::default()));
    for (at, row) in rows.iter().enumerate() {
        let want = runtime
            .block_on(old.evaluate(Variable::from(row.clone())))
            .map(|response| response.result.to_value());
        let rowed = batch[at]
            .as_ref()
            .map(|response| response.result.to_value());
        assert_eq!(want.is_ok(), rowed.is_ok(), "row {at}");
        let Ok(want) = want else {
            assert!(output.errors[at].is_some(), "row {at}");
            continue;
        };
        assert_eq!(rowed.as_ref().ok(), Some(&want), "row {at}");
        let got = output.row(at).to_value();
        for pointer in kept {
            let expected = want.pointer(pointer).cloned().unwrap_or(Value::Null);
            let actual = got.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(actual, expected, "row {at} {pointer}");
        }
    }
    paths
}

fn typed_and_kept(graph: Value, rows: Vec<Value>, kept: &[&str]) {
    let label = graph.to_string();
    let paths = columns_keep_version_one(graph, rows, kept);
    assert!(
        paths.typed > 0 && paths.laned == 0,
        "{label}: expected typed, got {paths:?}"
    );
}

fn composite_rows() -> Vec<Value> {
    vec![
        json!({"x": 1, "y": 2, "s": "a", "flag": true, "items": [1, 2, 3], "tags": ["p", "q"]}),
        json!({"x": null, "y": 2, "s": "b", "flag": false, "items": [], "tags": []}),
        json!({"x": 3, "y": -4, "s": null, "flag": null, "items": [5], "tags": ["r"]}),
        json!({"x": 0, "y": 0, "s": "", "flag": true, "items": [0, -1], "tags": ["", "s"]}),
        json!({"y": 1.5, "items": [2.25], "tags": ["t"]}),
        json!({"x": 2.5, "y": 1, "s": "Zz", "flag": false, "items": [7, 8, 9, 10], "tags": ["u", "v", "w"]}),
    ]
}

#[test]
fn an_object_read_by_later_entries_runs_typed() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            (
                "o",
                "{total: x + y, label: s, none: null, empty: {}, inner: {flag: flag, n: y * 2, deeper: {t: s ?? 'none'}}}",
            ),
            ("count", "len(filter(values($.o.inner), # == true))"),
            ("first", "$.o.inner.n + 1"),
            ("label", "$.o.label"),
            ("missing", "$.o.nothing"),
            ("deep", "$.o.inner.deeper.t"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/o", "/count", "/first", "/label", "/deep"]);
}

#[test]
fn an_object_with_failing_members_fails_its_rows_like_version_1() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("o", "{a: upper(s), b: x * 2, c: {d: x, e: y}}"),
            ("n", "count(values($.o), # != null)"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/o", "/n"]);
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("o", "({a: 1, b: x + 1}).a"),
            ("p", "({a: y, b: {c: x + y}}).b.c"),
            ("t", "({a: y, b: {c: x + y}}).b"),
            ("q", "len(values({a: s, b: x + 1}))"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/o", "/p", "/q", "/t"]);
}

#[test]
fn an_empty_object_runs_typed() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("o", "{}"),
            ("p", "x"),
            ("q", "{a: {}, b: {c: {}, d: y}}"),
            ("r", "$.q.b"),
            ("t", "{n: x} == {n: x} or {m: y} != null"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/q", "/r", "/t"]);
}

#[test]
fn values_and_keys_of_an_object_keep_version_1_order() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("o", "{b: x, a: s, c: {d: y}}"),
            ("v", "values($.o)"),
            ("k", "keys($.o)"),
            ("w", "values({z: 1, y: 2, x: 3})"),
            ("n", "len(keys({p: 1, q: flag}))"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/o", "/v", "/k", "/w", "/n"]);
}

#[test]
fn an_object_feeds_a_later_node_like_version_1() {
    let graph = chain(vec![
        entries(
            "n1",
            false,
            &[
                ("o", "{a: x + 1, b: s, c: {d: flag, e: null}, f: {}}"),
                ("k", "$.o.a"),
            ],
        ),
        entries(
            "n2",
            true,
            &[
                ("p", "o.a * 2"),
                ("q", "o.c.d"),
                ("r", "o.c.e ?? 'gone'"),
                ("t", "o.b"),
            ],
        ),
    ]);
    let paths = columns_keep_version_one(graph, composite_rows(), &["/o", "/p", "/q", "/r", "/t"]);
    assert!(paths.typed > 0, "{paths:?}");
}

#[test]
fn an_object_of_flags_counted_by_a_later_entry_runs_typed() {
    let graph = chain(vec![entries(
        "offers",
        true,
        &[
            (
                "eligibility.offers",
                "{bundle: x > 1, booster: y >= 0, family: len(items) >= 3, upgrade: flag, tier: s in ['a', 'Zz']}",
            ),
            (
                "eligibility.count",
                "len(filter(values($.eligibility.offers), # == true))",
            ),
            (
                "eligibility.any",
                "some(values($.eligibility.offers), # == true)",
            ),
        ],
    )]);
    typed_and_kept(
        graph,
        composite_rows(),
        &["/eligibility/offers", "/eligibility/count", "/eligibility/any"],
    );
}

#[test]
fn a_part_of_a_split_text_runs_typed() {
    let rows = vec![
        json!({"v": "1.2.3", "sep": "."}),
        json!({"v": "10.x", "sep": ""}),
        json!({"v": "", "sep": "x"}),
        json!({"v": null, "sep": "."}),
        json!({"v": "ab", "sep": null}),
        json!({"v": "é.ü", "sep": "."}),
        json!({"sep": "."}),
    ];
    for source in [
        "split(v, '.')[0]",
        "split(v, '.')[2]",
        "(split(v, sep))[1]",
        "split(v, '')[0]",
        "upper(split(v, '.')[1] ?? 'none')",
        "split('a-b-c', '-')[1] + (v ?? '')",
    ] {
        typed_like_version_one(expression(source), rows.clone());
    }
    let numbers = vec![
        json!({"v": "1.2.3"}),
        json!({"v": "10"}),
        json!({"v": "x.1"}),
        json!({"v": null}),
    ];
    typed_like_version_one(expression("number(split(v, '.')[0])"), numbers);
}

#[test]
fn member_chains_on_computed_objects_run_typed() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("o", "{a: {b: {c: x * 2}}, d: s}"),
            ("p", "$.o.a.b.c + 1"),
            ("q", "($.o.a).b.c"),
            ("r", "({k: {m: y}}).k.m"),
            ("t", "($.o).d ?? 'none'"),
            ("u", "$.o.a.zzz"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/o", "/p", "/q", "/r", "/t", "/u"]);
}

#[test]
fn an_array_literal_runs_typed() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("a", "[x, y + 1, s, null, [flag], {k: s}]"),
            ("b", "[]"),
            ("c", "[`rate ${y}`, s ?? 'none', upper(s ?? '')]"),
            ("d", "[[], [[]], {}]"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/a", "/b", "/c", "/d"]);
}

#[test]
fn closures_and_folds_over_an_array_literal_run_typed() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("flags", "filter([x > 0, y > 0, flag == true], # == true)"),
            ("count", "len($.flags)"),
            ("doubled", "map([x, y, 3], # * 2)"),
            ("kept", "filter(map([y, 3, -1], # + 1), # > 0)"),
            ("some", "some([x, y], # > 1)"),
            ("all", "all([x, y], # > 1)"),
            ("none", "none([s, 'q'], # == 'a')"),
            ("one", "one([x, y, 2], # == 2)"),
            ("counted", "count([s, 'a', 'b'], # == 'a')"),
            ("sum", "sum([x, y, 1]) + sum(filter([x, y], # > 0))"),
            ("max", "max([x, y]) - min([y, 0])"),
            ("avg", "avg([x, y])"),
            ("in", "s in [upper('a'), 'b', s ?? 'z']"),
            ("contains", "contains([y, x], 2)"),
            ("values", "len(values({p: x, q: [y]}))"),
        ],
    )]);
    typed_and_kept(
        graph,
        composite_rows(),
        &["/flags", "/count", "/doubled", "/kept", "/some", "/sum", "/in"],
    );
}

#[test]
fn flatten_runs_typed() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("a", "flatten([tags, [s, 'k'], items])"),
            ("b", "flatten([[1, [2]], 3, filter(items, # > 1)])"),
            ("c", "len(flatten([[x], [y, s]]))"),
            ("d", "flatten([])"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/a", "/b", "/c", "/d"]);
    let mut rows = composite_rows();
    rows.push(json!({"x": 1, "y": 2, "s": "n", "flag": true, "items": null, "tags": null}));
    columns_keep_version_one(
        chain(vec![entries("e", false, &[("a", "flatten([tags, items])")])]),
        rows,
        &["/a"],
    );
}

#[test]
fn an_array_feeds_a_later_node_like_version_1() {
    let graph = chain(vec![
        entries(
            "n1",
            false,
            &[
                ("reasons", "filter([s, 'fixed', x > 1 ? 'big' : null], # != null)"),
                ("summary", "[`total ${x + y}`, s]"),
                ("n", "len($.reasons)"),
            ],
        ),
        entries(
            "n2",
            true,
            &[
                ("first", "reasons[0]"),
                ("count", "len(reasons)"),
                ("has", "'fixed' in reasons"),
                ("decision", "{approved: n > 1, reasons: reasons}"),
            ],
        ),
    ]);
    let paths = columns_keep_version_one(
        graph,
        composite_rows(),
        &["/reasons", "/summary", "/first", "/count", "/has", "/decision"],
    );
    assert!(paths.typed > 0, "{paths:?}");
}

#[test]
fn membership_in_a_computed_array_runs_typed() {
    let rows = vec![
        json!({"a": "2024-01-15", "b": "2024-01-15", "c": "2024-02-01", "s": "x", "t": "x"}),
        json!({"a": "2024-03-01", "b": "2024-01-15", "c": null, "s": null, "t": "y"}),
        json!({"a": null, "b": "2024-01-15", "c": "2024-02-01", "s": "y", "t": null}),
        json!({"a": "2024-02-01", "b": null, "c": "2024-02-01", "s": "z", "t": "z"}),
    ];
    for source in [
        "date(a) in [date(b), date(c)]",
        "s in [t, upper(t ?? ''), 'x']",
        "s not in [t, 'q']",
        "contains([t, s], 'z')",
    ] {
        typed_like_version_one(expression(source), rows.clone());
    }
}

fn indexed_rows() -> Vec<Value> {
    vec![
        json!({"items": [{"price": 1.5, "on": true}, {"price": 2, "on": false}], "tags": ["a", "b"], "nums": [3, 4, 5], "i": 1}),
        json!({"items": [], "tags": [], "nums": [], "i": 0}),
        json!({"items": [{"price": null, "on": true}], "tags": ["x"], "nums": [1], "i": 2}),
        json!({"items": null, "tags": null, "nums": null, "i": null}),
        json!({"items": [{"on": false}, {"price": 7, "on": true}], "tags": ["p", "q", "r"], "nums": [0, -1], "i": 0}),
        json!({"items": [{"price": 3, "on": null}], "tags": ["s"], "nums": [2.5], "i": 1}),
    ]
}

#[test]
fn an_element_of_a_list_runs_typed() {
    for source in [
        "items[0].price",
        "items[1].on",
        "tags[1]",
        "nums[2] ?? 99",
        "nums[0] * 2",
        "filter(items ?? [], #.on == true)[0].price",
        "filter(nums ?? [], # > 0)[1]",
        "upper(tags[0] ?? 'none')",
    ] {
        typed_like_version_one(expression(source), indexed_rows());
    }
    columns_match_version_one(expression("filter(items, #.on)[0].price"), indexed_rows());
}

#[test]
fn an_element_of_an_array_literal_runs_typed() {
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("a", "[x, y][1]"),
            ("b", "[x, y + 1][5]"),
            ("c", "([s, 'k'])[0] ?? 'none'"),
            ("d", "[[x], {k: y}][1].k"),
            ("e", "[x + 1, 2][1]"),
        ],
    )]);
    typed_and_kept(graph, composite_rows(), &["/a", "/b", "/c", "/d", "/e"]);
}

#[test]
fn an_element_of_a_list_written_by_an_earlier_node_runs_typed() {
    let graph = chain(vec![
        entries(
            "n1",
            false,
            &[
                ("reasons", "filter(tags, # != 'q')"),
                ("prices", "map(items, #.price)"),
            ],
        ),
        entries(
            "n2",
            true,
            &[("first", "reasons[0]"), ("second", "prices[1] ?? 0")],
        ),
    ]);
    let rows: Vec<Value> = indexed_rows()
        .into_iter()
        .filter(|row| !row["items"].is_null())
        .collect();
    let paths = columns_keep_version_one(graph, rows, &["/first", "/second", "/reasons"]);
    assert!(paths.typed > 0, "{paths:?}");
}

#[test]
fn a_value_compared_with_a_list_runs_typed() {
    let rows = vec![
        json!({"loc": "DE", "home": "US", "frequent": ["CA", "UK"], "n": 1}),
        json!({"loc": "US", "home": "US", "frequent": [], "n": 2}),
        json!({"loc": null, "home": "US", "frequent": null, "n": 3}),
        json!({"loc": "CA", "home": null, "frequent": ["CA"], "n": 4}),
        json!({"loc": "FR", "home": "US", "frequent": null, "n": 5}),
    ];
    let table = graph(
        json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [{"id": "i0", "field": "loc"}, {"id": "i1", "field": "n"}],
            "outputs": [{"id": "o0", "field": "out"}],
            "rules": [
                {"_id": "r0", "i0": "!= home and != frequent", "i1": "> 3", "o0": "'away'"},
                {"_id": "r1", "i0": "== frequent", "i1": "", "o0": "'same'"},
                {"_id": "r2", "i0": "", "i1": "", "o0": "'other'"}
            ]
        }}),
    );
    typed_like_version_one(table, rows.clone());
    for source in ["loc != frequent", "frequent == loc", "(frequent ?? []) != n"] {
        typed_like_version_one(expression(source), rows.clone());
    }
}

#[test]
fn membership_in_a_list_that_is_always_missing_runs_typed() {
    let rows = vec![
        json!({"tier": "high", "age": 70}),
        json!({"tier": "low", "age": 10}),
        json!({"tier": null, "age": 40}),
    ];
    let graph = chain(vec![
        entries("clear", true, &[("patient", "null"), ("next", "age + 1")]),
        json!({"id": "t", "name": "t", "type": "decisionTableNode", "content": {
            "hitPolicy": "collect",
            "passThrough": true,
            "outputPath": "providers",
            "inputs": [{"id": "i0", "field": "tier"}, {"id": "i1"}],
            "outputs": [{"id": "o0", "field": "role"}],
            "rules": [
                {"_id": "r0", "i0": "'high'", "i1": "", "o0": "'physician'"},
                {"_id": "r1", "i0": "", "i1": "'diabetes' in patient.conditions", "o0": "'specialist'"},
                {"_id": "r2", "i0": "", "i1": "'x' not in patient.conditions", "o0": "'none'"},
                {"_id": "r3", "i0": "", "i1": "age > 60", "o0": "'geriatric'"}
            ]
        }}),
    ]);
    let paths = columns_keep_version_one(graph, rows, &["/providers"]);
    assert!(paths.typed > 0 && paths.laned == 0, "{paths:?}");
}

#[test]
fn a_collect_table_with_computed_outputs_runs_typed() {
    let inputs = json!([{"id": "i0", "field": "tier"}, {"id": "i1", "field": "n"}, {"id": "i2"}]);
    let outputs = json!([
        {"id": "o0", "field": "points"},
        {"id": "o1", "field": "label"},
        {"id": "o2", "field": "detail.size"}
    ]);
    let rules = vec![
        json!({"_id": "r0", "i0": "'gold'", "i1": "", "i2": "", "o0": "n * 10", "o1": "'gold'", "o2": "len(tags)"}),
        json!({"_id": "r1", "i0": "", "i1": "> 3", "i2": "", "o0": "5", "o1": "upper(s)", "o2": ""}),
        json!({"_id": "r2", "i0": "'silver', 'bronze'", "i1": "", "i2": "flag == true", "o0": "", "o1": "", "o2": ""}),
        json!({"_id": "r3", "i0": "", "i1": "[0..10]", "i2": "", "o0": "n + 1", "o1": "null", "o2": "n * 2"}),
        json!({"_id": "r4", "i0": "", "i1": "", "i2": "contains(tags, 'x')", "o0": "100", "o1": "'x'", "o2": "1"}),
    ];
    for path in [None, Some("hits"), Some("result.hits")] {
        let graph = collect_table(path, inputs.clone(), outputs.clone(), rules.clone());
        let label = graph.to_string();
        let paths = columns_keep_version_one(
            graph,
            collect_rows(),
            &["", "/hits", "/result/hits"],
        );
        assert!(paths.typed > 0 && paths.laned == 0, "{label}: {paths:?}");
    }
}

#[test]
fn membership_in_a_list_of_entries_written_earlier_in_the_node_runs_typed() {
    let rows = vec![
        json!({"o": null}),
        json!({"o": {"s": "x"}}),
        json!({"o": {"s": "2024-01-15 10:00:00.123456789"}}),
    ];
    let graph = chain(vec![entries(
        "e",
        false,
        &[
            ("k0", "dateString('2024-01-15T10:00:00.123456789Z')"),
            ("k1", "((o.s ?? $.k0) ?? $.k0) in [$.k0]"),
            ("k2", "o.s in [$.k0, 'x']"),
            ("k3", "$.k0 in ['x', $.k0]"),
        ],
    )]);
    typed_and_kept(graph, rows, &["/k1", "/k2", "/k3"]);
}

#[test]
fn a_division_that_overflows_compares_like_version_1() {
    let rows = vec![
        json!({"a": -79228162514264337593543950335_f64, "s": null}),
        json!({"a": 1, "s": "x"}),
        json!({"a": 7.5, "s": null}),
    ];
    for source in [
        "s in [(a / 0.1)]",
        "s == a / 0.1",
        "(a / 0.5) ?? 0",
        "a / 2 == null",
    ] {
        typed_like_version_one(expression(source), rows.clone());
    }
}

#[test]
fn a_failing_inner_filter_fails_under_an_outer_filter_that_keeps_nothing() {
    let rows = vec![
        json!({"t": "a", "s": "q"}),
        json!({"t": "b", "s": null}),
    ];
    for source in [
        "filter(filter([t], contains(null, '10%')), false)",
        "filter(filter([t, s], upper(#) != 'Q'), false)",
        "len(filter(filter([t, s], # != 'q'), false))",
    ] {
        columns_match_version_one(expression(source), rows.clone());
    }
}

#[test]
fn a_closure_over_a_missing_list_fails_even_with_a_fallback() {
    let rows = vec![
        json!({"objs": null, "nums": null}),
        json!({"objs": [{"x": 1, "on": true}, {"x": 2, "on": false}], "nums": [1, 2]}),
    ];
    for source in [
        "map(objs, #.x) ?? []",
        "len(filter(objs, #.on) ?? [])",
        "sum(map(nums, # * 2) ?? [])",
    ] {
        columns_match_version_one(expression(source), rows.clone());
    }
    typed_like_version_one(expression("len(filter(objs ?? [], #.on))"), rows);
}

#[test]
fn an_index_beyond_any_list_fails_like_version_1() {
    for source in ["[][79228162514264337593543950335]", "[x, 2][1e28]", "[x, 2][3]"] {
        columns_match_version_one(expression(source), composite_rows());
    }
}

#[test]
fn lists_written_by_a_table_feed_flatten_typed() {
    let rows = vec![
        json!({"device": "mobile", "tier": "gold"}),
        json!({"device": "tablet", "tier": "none"}),
        json!({"device": "desktop", "tier": "gold"}),
        json!({"device": "watch", "tier": "silver"}),
        json!({"device": null, "tier": null}),
    ];
    let device = json!({"id": "t1", "name": "t1", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "passThrough": true,
        "inputs": [{"id": "i0", "field": "device"}],
        "outputs": [{"id": "o0", "field": "p.ui"}, {"id": "o1", "field": "p.layout"}],
        "rules": [
            {"_id": "r0", "i0": "'mobile'", "o0": "['compact', 'touch']", "o1": "{ 'type': 'compact' }"},
            {"_id": "r1", "i0": "'tablet'", "o0": "['touch', 'medium']", "o1": "{ 'type': 'medium' }"},
            {"_id": "r2", "i0": "'desktop'", "o0": "['expanded']", "o1": "{ 'type': 'full' }"}
        ]
    }});
    let loyalty = json!({"id": "t2", "name": "t2", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "passThrough": true,
        "inputs": [{"id": "i0", "field": "tier"}],
        "outputs": [{"id": "o0", "field": "p.features"}],
        "rules": [
            {"_id": "r0", "i0": "'gold'", "o0": "['priority', 'deals']"},
            {"_id": "r1", "i0": "", "o0": "['standard']"}
        ]
    }});
    let graph = chain(vec![
        device,
        loyalty,
        entries(
            "e",
            true,
            &[
                ("all", "flatten([p.features, p.ui ?? []])"),
                ("count", "len(p.features)"),
                ("first", "p.features[0]"),
            ],
        ),
    ]);
    let paths = columns_keep_version_one(graph, rows, &["/all", "/count", "/first"]);
    assert!(paths.typed > 0 && paths.laned == 0, "{paths:?}");
}

fn sample_graph(name: &str, count: usize) -> (Arc<DecisionContent>, Vec<Value>) {
    fn vary(value: &Value, i: usize) -> Value {
        match value {
            Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(x), _) => {
                    Value::from(x + (i % 13) as i64 * x.signum() * (x.abs() / 10).max(1))
                }
                (_, Some(x)) => Value::from(x * (1.0 + (i % 13) as f64 / 20.0)),
                _ => value.clone(),
            },
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), vary(v, i))).collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(|v| vary(v, i)).collect()),
            other => other.clone(),
        }
    }
    let path = format!(
        "{}/../../test-data/graphs/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let tests: Vec<Value> = doc["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|test| test["input"].clone())
        .collect();
    let rows = (0..count)
        .map(|i| vary(&tests[i % tests.len()], i / tests.len()))
        .collect();
    (Arc::new(serde_json::from_value(doc).unwrap()), rows)
}

#[test]
fn the_same_columns_take_the_same_paths_on_every_call() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for name in [
        "application-risk-assessment",
        "policy-eligibility-analyzer",
        "booking-personalization-system",
        "school-district-resource-allocation",
        "loan-approval",
        "customer-lifetime-value",
        "order-consolidation-system",
    ] {
        let (content, rows) = sample_graph(name, 2000);
        let built = Built::nested(&rows)
            .or_else(|| Built::lists(&rows))
            .or_else(|| Built::new(&rows))
            .unwrap();
        let fields = built.fields();
        let structs = built.structs(&fields);
        let columns = built.columns_with(&structs);
        let fresh = || {
            DecisionEngine::default()
                .with_version(EngineVersion::V2)
                .create_decision(content.clone())
                .unwrap()
        };
        let decision = fresh();
        let paths = |decision: &zen_engine::Decision| {
            runtime
                .block_on(decision.evaluate_columns(&columns, EvaluationOptions::default()))
                .paths()
        };
        let first = paths(&decision);
        assert!(first.typed > 0, "{name}: {first:?}");
        for call in 1..24 {
            assert_eq!(paths(&decision), first, "{name}: call {call}");
        }
        for round in 0..4 {
            let other = fresh();
            for call in 0..3 {
                assert_eq!(
                    paths(&other),
                    first,
                    "{name}: fresh decision {round}, call {call}"
                );
            }
        }
    }
}

fn bracket_rows() -> Vec<Value> {
    vec![
        json!({"s": "months", "n": 5, "flag": null, "z": 4}),
        json!({"s": "", "n": -1.5, "flag": true, "z": null}),
        json!({"s": null, "n": null, "flag": false, "z": 0}),
    ]
}

#[test]
fn an_empty_key_on_a_value_written_earlier_reads_null() {
    for (value, key) in [
        ("'months'", "''"),
        ("s", "''"),
        ("upper(s ?? 'x')", "''"),
        ("n + 1", "''"),
        ("flag == true", "''"),
        ("date('2024-01-15')", "''"),
        ("s", "'a'"),
        ("s", "0"),
        ("n * 2", "'a'"),
    ] {
        let graph = chain(vec![entries(
            "e",
            false,
            &[
                ("u", value),
                ("o", &format!("u[{key}]")),
                ("p", &format!("u[{key}] ?? 'fb'")),
            ],
        )]);
        columns_match_version_one(graph, bracket_rows());
        let graph = chain(vec![
            entries("first", true, &[("u", value)]),
            entries("second", false, &[("o", &format!("u[{key}]"))]),
        ]);
        columns_match_version_one(graph, bracket_rows());
    }
}

#[test]
fn an_index_past_the_end_fails_when_an_element_fails() {
    for source in [
        "bool(map([3 / 11.97, flag ? 1 : -615, z ?? 2], # > 1)[99])",
        "bool([3 / 11.97, flag ? 1 : -615][5])",
        "bool(filter([1, flag ? 1 : -615], # > 0)[5])",
        "bool(map([1, 2], flag ? # : 0)[7])",
        "bool([[1], [flag ? 1 : 2]][3])",
        "bool([{a: 1}, {a: flag ? 1 : 2}][3])",
        "bool({a: null, b: flag ? 1 : 2}.a)",
        "bool({a: 1, b: flag ? 1 : 2}.c)",
        "bool(flag ? null : null)",
    ] {
        columns_match_version_one(expression(source), bracket_rows());
    }
}

fn text_rows(count: usize, distinct: bool) -> Vec<Value> {
    let words = ["Sulfuric Acid", "Toxic gas", "", "flammable LIQUID", "rock"];
    (0..count)
        .map(|i| {
            let word = words[i % words.len()];
            let s = match (distinct, i % 7) {
                (_, 3) => Value::Null,
                (true, _) => Value::from(format!("{word} {i}")),
                (false, _) => Value::from(word),
            };
            let t = match i % 5 {
                0 => Value::from(format!("{:02}:{:02}", i % 24, i % 60)),
                1 => Value::from("23:59"),
                2 => Value::from("not a time"),
                3 => Value::Null,
                _ => Value::from(7),
            };
            json!({"s": s, "t": t, "w": words[(i / 2) % words.len()], "n": i % 4, "flag": i % 3 == 0})
        })
        .collect()
}

#[test]
fn text_work_on_one_column_runs_once_per_distinct_value_like_version_1() {
    for distinct in [false, true] {
        for count in [40, 300] {
            let rows = text_rows(count, distinct);
            for source in [
                "contains(lower(s), 'acid') or contains(lower(s), 'toxic')",
                "upper(trim(s)) + '!'",
                "len(s) > 4 and n > 1",
                "startsWith(s, 'Tox') ? n : len(s)",
                "flag ? lower(s) : upper(s)",
                "time(t) > 3600",
                "lower(s) == 'rock' or time(t) < 60",
                "contains(lower(s), 'gas') and contains(upper(s), 'TOXIC')",
                "contains(lower(s), 'a') and contains(lower(w), 'c')",
                "lower(s) + '/' + upper(w)",
                "len(w) > len(s ?? '')",
            ] {
                columns_match_version_one(expression(source), rows.clone());
            }
        }
    }
}

#[test]
fn text_cells_on_one_column_run_once_per_distinct_value_like_version_1() {
    let graph = graph(json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "inputs": [{"id": "i0", "field": "lower(s)"}, {"id": "i1", "field": "n"}, {"id": "i2", "field": "t"}],
        "outputs": [{"id": "o0", "field": "out"}, {"id": "o1", "field": "label"}],
        "rules": [
            {"_id": "r0", "i0": "some(['acid', 'base'], contains($, #))", "i1": "> 1", "i2": "", "o0": "'r0'", "o1": "upper(s)"},
            {"_id": "r1", "i0": "contains($, 'toxic')", "i1": "", "i2": "time($) > 600", "o0": "'r1'", "o1": "lower(s) + '?'"},
            {"_id": "r2", "i0": "startsWith($, 'flam')", "i1": "", "i2": "", "o0": "'r2'", "o1": "'x'"},
            {"_id": "r3", "i0": "", "i1": "", "i2": "", "o0": "'r3'", "o1": "trim(s)"}
        ]
    }}));
    for distinct in [false, true] {
        for count in [40, 300] {
            columns_match_version_one(graph.clone(), text_rows(count, distinct));
        }
    }
}

#[test]
fn wide_decimals_written_by_one_node_are_read_typed_by_the_next() {
    let rows: Vec<Value> = (0..120)
        .map(|i| match i % 9 {
            4 => json!({"d": null, "m": 6000 + i}),
            7 => json!({"d": 1800 + i, "m": 0}),
            _ => json!({"d": 1800 + i, "m": 6000 + 3 * i}),
        })
        .collect();
    let graph = chain(vec![
        entries("first", true, &[("r", "(d / m) * 100"), ("q", "d / 7")]),
        entries(
            "second",
            false,
            &[
                ("rate", "5.5 + ((100 - r) * 0.05)"),
                ("big", "r > 30"),
                ("sum", "r + q"),
                ("same", "r"),
            ],
        ),
    ]);
    let paths = columns_match_version_one(graph, rows);
    assert!(paths.typed >= 2, "{paths:?}");
}

#[test]
fn failing_rows_of_a_deduplicated_unit_fail_like_version_1() {
    let rows: Vec<Value> = (0..96)
        .map(|i| match i % 4 {
            0 => json!({"s": "a", "n": 1, "d": 1801, "m": 6003}),
            1 => json!({"s": "b", "n": null, "d": 1, "m": 3}),
            2 => json!({"s": null, "n": 2, "d": 2, "m": 0}),
            _ => json!({"s": "c", "n": "x", "d": 7, "m": 9}),
        })
        .collect();
    for source in [
        "upper(s) + string(n + 1)",
        "[upper(s), lower(s)]",
        "{k: upper(s), v: n * 2}",
        "upper(s) + string(d / m * 100)",
    ] {
        columns_match_version_one(expression(source), rows.clone());
    }
    let graph = chain(vec![
        entries("first", true, &[("t", "upper(s) + '!'"), ("r", "(d / m) * 100")]),
        entries(
            "second",
            false,
            &[("u", "lower(t) + string(n + 1)"), ("w", "r * n"), ("z", "t + string(r)")],
        ),
    ]);
    columns_match_version_one(graph, rows);
}

#[test]
fn a_date_list_with_an_item_that_fails_fails_even_when_an_earlier_item_matches() {
    let rows = vec![
        json!({"ds": "2023-01-31"}),
        json!({"ds": "2024-02-29"}),
        json!({"ds": null}),
    ];
    for source in [
        "d(ds) in [d('2023-01-31'), d('2025-03-30 02:30', '+05:30')]",
        "d(ds) not in [d('2023-01-31'), d('2025-03-30 02:30', '+05:30')]",
        "d(ds) in [d('2023-01-31'), d('2025-03-30 02:30', 'Mars/Base')]",
        "d(ds) in [d('2025-03-30 02:30', '+05:30'), d('2023-01-31')]",
    ] {
        columns_match_version_one(expression(source), rows.clone());
    }
    let table = graph(json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "inputs": [{"id": "i0", "field": "d(ds)"}],
        "outputs": [{"id": "o0", "field": "out"}],
        "rules": [
            {"_id": "r0", "i0": "[d('2023-01-31'), d('2023-01-31'), d('2025-03-30 02:30', '+05:30')]", "o0": "'r0'"},
            {"_id": "r1", "i0": "", "o0": "'r1'"}
        ]
    }}));
    columns_match_version_one(table, rows);
}

fn scaled_numbers() -> Vec<Value> {
    [
        json!(2.5),
        json!(-2.5),
        json!(-0.5),
        json!(0.5),
        json!(-0.04),
        json!(7),
        json!(-7),
        json!(0),
        json!(1.005),
        json!(-1.005),
        json!(123456.789123),
        json!(-0.000001),
        json!(9.99),
        json!(1.25),
        json!(-1.25),
        json!(null),
    ]
    .into_iter()
    .enumerate()
    .map(|(at, x)| json!({"x": x, "k": at % 3 + 1}))
    .collect()
}

#[test]
fn rounding_functions_over_mixed_scales_run_typed() {
    for source in [
        "floor(x)",
        "ceil(x)",
        "round(x)",
        "round(x, 2)",
        "round(x * k, 1)",
        "trunc(x)",
        "trunc(x, 2)",
        "abs(x)",
        "floor(x * k * 1.5)",
        "ceil(x / 4)",
    ] {
        typed_like_version_one(expression(source), scaled_numbers());
    }
}

#[test]
fn any_all_and_none_over_lists_keep_version_1_errors() {
    let rows = vec![
        json!({"nums": [1, 2, 3], "n": 2}),
        json!({"nums": [], "n": 2}),
        json!({"nums": [5, null, 1], "n": 1}),
        json!({"nums": [null, 1], "n": 1}),
        json!({"nums": [1, null], "n": 1}),
        json!({"nums": [4, 4, 4], "n": 4}),
        json!({"nums": [0, 9], "n": null}),
        json!({"nums": null, "n": 3}),
    ];
    for source in [
        "some(nums, # > n)",
        "all(nums, # > n)",
        "none(nums, # > n)",
        "some(nums, # == 1)",
        "all(nums, # != 9)",
        "contains(nums, n)",
        "contains(nums ?? [], 4)",
        "some(filter(nums, # > 0), # > n)",
        "all(filter(nums, # != 4), # < 6)",
    ] {
        columns_match_version_one(expression(source), rows.clone());
    }
    let texts = vec![
        json!({"tags": ["kidney_disease", "copd"]}),
        json!({"tags": ["a"]}),
        json!({"tags": []}),
        json!({"tags": null}),
        json!({"tags": ["copd", null]}),
    ];
    for source in [
        "contains(tags ?? [], 'kidney_disease') or contains(tags ?? [], 'copd')",
        "some(tags ?? [], # == 'a')",
        "none(tags ?? [], # == 'copd')",
    ] {
        columns_match_version_one(expression(source), texts.clone());
    }
}

#[test]
fn a_batch_where_most_rows_fail_matches_version_1() {
    let rows: Vec<Value> = (0..4800)
        .map(|at| match at % 10 {
            0 => json!({"n": at, "m": at % 7, "s": format!("k{at}")}),
            _ => json!({"n": null, "m": at % 7, "s": format!("k{at}")}),
        })
        .collect();
    columns_match_version_one(expression("n * 2 + m"), rows.clone());
    columns_match_version_one(
        expression("len(s) > 2 ? n / (m + 1) : m"),
        rows.clone(),
    );
    let table = graph(json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "inputs": [{"id": "i0", "field": "s"}],
        "outputs": [{"id": "o0", "field": "out"}],
        "rules": [
            {"_id": "r0", "i0": "'k0'", "o0": "m + 1"},
            {"_id": "r1", "i0": "'k10'", "o0": "n * 3"},
            {"_id": "r2", "i0": "", "o0": "n * m"}
        ]
    }}));
    columns_match_version_one(table, rows);
}

#[test]
fn keys_with_constant_presence_group_rows_like_version_1() {
    let rows: Vec<Value> = (0..600)
        .map(|at| {
            json!({
                "a": (at % 3) as f64 * 1.5,
                "b": (at % 5 != 0).then_some(at % 4),
                "c": at % 2 == 0,
                "t": (["x", "y", "z"])[at % 3],
                "d": 10
            })
        })
        .collect();
    typed_like_version_one(
        expression("(a * 2 + d) * (c ? 1.25 : 0.75) + (t == 'x' ? 1 : 2) + (b ?? 0)"),
        rows,
    );
}

#[test]
fn legacy_dates_of_distinct_texts_run_typed() {
    let rows: Vec<Value> = (0..40)
        .map(|at| match at % 8 {
            0 => json!({"t": format!("2025-03-{:02}T10:{:02}:00Z", at % 28 + 1, at % 60), "u": "2025-01-01"}),
            1 => json!({"t": format!("2024-{:02}-{:02}", at % 12 + 1, at % 28 + 1), "u": "2025-01-01T00:00:00Z"}),
            2 => json!({"t": "not a date", "u": "2025-01-01"}),
            3 => json!({"t": null, "u": "2025-01-01"}),
            4 => json!({"t": format!("2023-02-{:02} 08:30:00", at % 28 + 1), "u": "2024-12-31"}),
            5 => json!({"t": "2025-13-40", "u": "2025-01-01"}),
            6 => json!({"t": format!("2025-03-{:02}T10:00:00+02:00", at % 28 + 1), "u": "2025-06-01"}),
            _ => json!({"t": "", "u": "2025-01-01"}),
        })
        .collect();
    for source in [
        "(date(t) - date(u)) / 3600",
        "date(t) > date(u)",
        "date(t)",
    ] {
        columns_match_version_one(expression(source), rows.clone());
    }
}

#[test]
fn trim_and_type_of_distinct_values_run_typed() {
    let rows: Vec<Value> = (0..32)
        .map(|at| match at % 8 {
            0 => json!({"s": format!("  id{at}  "), "n": at}),
            1 => json!({"s": format!("\tid{at}\n"), "n": null}),
            2 => json!({"s": format!("\u{0B}id{at}\u{0C}\r"), "n": at as f64 / 4.0}),
            3 => json!({"s": format!("\u{a0}id{at}\u{2003}"), "n": -at}),
            4 => json!({"s": null, "n": 0}),
            5 => json!({"s": "   ", "n": null}),
            6 => json!({"s": "", "n": 1.5}),
            _ => json!({"s": format!("x{at}"), "n": at}),
        })
        .collect();
    for source in [
        "len(trim(s ?? ''))",
        "trim(s ?? '') == 'id8'",
        "type(n) != 'number'",
        "type(n) == 'null' ? 0 : n * 2",
    ] {
        typed_like_version_one(expression(source), rows.clone());
    }
    for source in ["type(n)", "type(s)", "trim(s ?? '') + '|'"] {
        columns_match_version_one(expression(source), rows.clone());
    }
}

#[test]
fn comparisons_and_ranges_over_mixed_scales_run_typed() {
    let values = [
        json!(0.5),
        json!(0.50),
        json!(1),
        json!(1.0),
        json!(2.99999999999),
        json!(3),
        json!(3.000000000001),
        json!(-1.25),
        json!(-0.0001),
        json!(1234567.123456),
        json!(0.0000000000000000001),
        json!(99999999999.5),
        json!(null),
    ];
    let rows: Vec<Value> = values
        .iter()
        .enumerate()
        .map(|(at, x)| json!({"x": x, "y": at as f64 * 0.75}))
        .collect();
    for source in [
        "x < 3",
        "x >= 3",
        "x > 2.5",
        "x <= 0.5",
        "x == 1",
        "x != 3",
        "x in [1..3]",
        "x in (1..3)",
        "x in [0.5..3.000000000001)",
        "x in (-1.25..0.5]",
        "x not in [1..3]",
        "y > 2.25 and x < 3",
    ] {
        typed_like_version_one(expression(source), rows.clone());
    }
}

#[test]
fn numbers_at_the_edges_of_a_mantissa_keep_version_1_results() {
    let edges = [
        "9223372036854775807",
        "-9223372036854775808",
        "9223372036854775808",
        "-9223372036854775809",
        "0",
        "-0.0",
        "0.0000000000000000000000000001",
        "-1.5",
        "79228162514264337593543950335",
    ];
    for at in 0..edges.len() {
        let rows: Vec<Value> = edges
            .iter()
            .cycle()
            .skip(at)
            .take(edges.len())
            .enumerate()
            .map(|(k, x)| {
                serde_json::from_str(&format!("{{\"x\": {x}, \"k\": {}}}", k % 3)).unwrap()
            })
            .collect();
        for source in ["x > 0", "x == 0", "x + k", "x * 2 > 1", "x"] {
            columns_match_version_one(expression(source), rows.clone());
        }
    }
}

fn dollar_table(rules: Value) -> Value {
    graph(json!({"id": "node", "name": "node", "type": "decisionTableNode", "content": {
        "hitPolicy": "first",
        "inputs": [
            {"id": "i0", "field": "level"},
            {"id": "i1"},
            {"id": "i2", "field": "$"}
        ],
        "outputs": [{"id": "o0", "field": "out"}],
        "rules": rules
    }}))
}

fn dollar_rows() -> Vec<Value> {
    (0..24)
        .map(|at| {
            json!({
                "level": ([json!("high"), json!("medium"), json!("low"), json!(null)])[at % 4].clone(),
                "n": at % 7,
            })
        })
        .collect()
}

#[test]
fn a_table_field_of_dollar_after_a_filled_column_runs_typed() {
    let rules = json!([
        {"_id": "r0", "i0": "\"high\"", "i1": "n > 3", "i2": "len(string(n)) > 0", "o0": "'a'"},
        {"_id": "r1", "i0": "\"high\", \"medium\"", "i1": "", "i2": "$ == \"medium\"", "o0": "'b'"},
        {"_id": "r2", "i0": "\"low\"", "i1": "", "i2": "", "o0": "'c'"},
        {"_id": "r3", "i0": "", "i1": "", "i2": "", "o0": "'d'"}
    ]);
    typed_like_version_one(dollar_table(rules), dollar_rows());
}

#[test]
fn a_table_field_of_dollar_without_a_filled_column_keeps_version_1_results() {
    let rules = json!([
        {"_id": "r0", "i0": "\"high\"", "i1": "n > 3", "i2": "", "o0": "'a'"},
        {"_id": "r1", "i0": "", "i1": "n > 1", "i2": "$ == \"medium\"", "o0": "'b'"},
        {"_id": "r2", "i0": "\"low\"", "i1": "", "i2": "$ == null", "o0": "'c'"},
        {"_id": "r3", "i0": "", "i1": "", "i2": "", "o0": "'d'"}
    ]);
    columns_match_version_one(dollar_table(rules), dollar_rows());
}

#[test]
fn membership_in_the_list_being_iterated_runs_typed() {
    let numbers = vec![
        json!({"nums": [1, 2, 4], "n": 1}),
        json!({"nums": [3, 6, 1.5, 3.0], "n": 2}),
        json!({"nums": [], "n": 3}),
        json!({"nums": [null, 2, 1], "n": 4}),
        json!({"nums": [0.5, 1, 1.00, 2.000], "n": 5}),
        json!({"nums": [5], "n": 6}),
        json!({"nums": [10, 20, 40, 80, 160], "n": 7}),
    ];
    for source in [
        "filter(nums, # * 2 in nums)",
        "filter(nums, # * 2 not in nums)",
        "some(nums, # + 1 in nums)",
        "count(nums, # + 1 in nums)",
        "all(nums, # / 2 in nums)",
        "map(nums, (# + n) in nums)",
    ] {
        typed_like_version_one(expression(source), numbers.clone());
    }
    let texts = vec![
        json!({"strs": ["a", "A", "b"]}),
        json!({"strs": ["B", "b"]}),
        json!({"strs": []}),
        json!({"strs": ["x", null]}),
        json!({"strs": ["q"]}),
    ];
    for source in ["filter(strs, upper(#) in strs)", "some(strs, lower(#) not in strs)"] {
        typed_like_version_one(expression(source), texts.clone());
    }
    let mixed = vec![
        json!({"nums": [1, 2, 4]}),
        json!({"nums": [1, "2", 4]}),
        json!({"nums": null}),
    ];
    columns_match_version_one(expression("filter(nums, # * 2 in nums)"), mixed);
}

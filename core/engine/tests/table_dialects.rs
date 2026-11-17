mod compiled_default;
mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use zen_engine::model::DecisionContent;
use zen_engine::policy::{EvaluateRequest, PolicyWorkspace};
use zen_engine::{Decision, EvaluationOptions};
use zen_expression::Variable;

struct Table {
    hit: &'static str,
    inputs: Vec<Option<&'static str>>,
    outputs: Vec<&'static str>,
    rules: Vec<Vec<&'static str>>,
    attributes: Value,
    models: Value,
    policy: Option<(Value, Value)>,
}

#[derive(Debug)]
struct Seen {
    graph: Vec<(&'static str, String)>,
    policy: Vec<(&'static str, String)>,
    graph_trace: String,
    policy_trace: String,
    columns: Option<(String, usize)>,
}

impl Seen {
    fn graph(&self) -> &str {
        &self.graph[0].1
    }

    fn policy(&self) -> &str {
        &self.policy[0].1
    }

    fn agreed(&self) -> &Self {
        for (mode, value) in &self.graph {
            assert_eq!(
                value,
                self.graph(),
                "graph mode {mode} disagrees: {self:#?}"
            );
        }
        for (mode, value) in &self.policy {
            match *mode {
                "driver" => assert_eq!(
                    Self::unordered(value),
                    Self::unordered(self.policy()),
                    "policy mode {mode} disagrees: {self:#?}"
                ),
                _ => assert_eq!(
                    value,
                    self.policy(),
                    "policy mode {mode} disagrees: {self:#?}"
                ),
            }
        }
        self
    }

    fn unordered(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
    }

    fn released(mut self, walker: Option<&str>, driver: Option<&str>) -> Self {
        for (list, mode, release) in [
            (&mut self.graph, "walker", walker),
            (&mut self.policy, "driver", driver),
        ] {
            if let Some(release) = release {
                let at = list
                    .iter()
                    .position(|(name, _)| *name == mode)
                    .expect("release mode");
                assert_eq!(
                    Self::unordered(&list.remove(at).1),
                    Self::unordered(release),
                    "release {mode}"
                );
            }
        }
        self
    }

    fn expect(&self, graph: &str, policy: &str) {
        println!(
            "graph  {}\npolicy {}\ngraph trace  {}\npolicy trace {}\ncolumns {:?}\n",
            self.graph(),
            self.policy(),
            self.graph_trace,
            self.policy_trace,
            self.columns
        );
        self.agreed();
        if let Some((columns, _)) = &self.columns {
            let parsed = |text: &str| {
                serde_json::from_str::<Value>(text)
                    .map_or_else(|_| Value::String(text.to_string()), Built::normalized)
            };
            assert_eq!(
                parsed(columns),
                parsed(self.policy()),
                "policy columns disagree: {self:#?}"
            );
        }
        assert_eq!((self.graph(), self.policy()), (graph, policy), "{self:#?}");
    }

    fn columnar(&self) -> &Self {
        assert_eq!(
            self.columns.as_ref().map(|(_, hosted)| *hosted),
            Some(0),
            "policy table was hosted: {self:#?}"
        );
        self
    }
}

impl Table {
    fn new(
        hit: &'static str,
        inputs: &[Option<&'static str>],
        outputs: &[&'static str],
        rules: &[&[&'static str]],
    ) -> Self {
        Self {
            hit,
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            rules: rules.iter().map(|r| r.to_vec()).collect(),
            attributes: json!({}),
            models: json!({"n": ["number", true]}),
            policy: None,
        }
    }

    fn with_attributes(mut self, attributes: Value) -> Self {
        self.attributes = attributes;
        self
    }

    fn with_models(mut self, models: Value) -> Self {
        self.models = models;
        self
    }

    fn with_policy(mut self, doc: Value, input: Value) -> Self {
        self.policy = Some((doc, input));
        self
    }

    fn prefixed(text: &str, prefix: &str) -> String {
        text.replace("{p}", prefix)
    }

    fn data(&self, prefix: &str, out_prefix: &str) -> Value {
        let inputs: Vec<Value> = self
            .inputs
            .iter()
            .enumerate()
            .map(|(i, field)| match field {
                Some(field) => {
                    json!({"id": format!("i{i}"), "name": "", "field": format!("{prefix}{field}")})
                }
                None => json!({"id": format!("i{i}"), "name": ""}),
            })
            .collect();
        let outputs: Vec<Value> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, field)| json!({"id": format!("o{i}"), "name": "", "field": format!("{out_prefix}{field}")}))
            .collect();
        let rules: Vec<Value> = self
            .rules
            .iter()
            .enumerate()
            .map(|(r, row)| {
                let mut rule = serde_json::Map::new();
                rule.insert("_id".into(), json!(format!("r{r}")));
                rule.insert("_description".into(), json!(format!("rule {r}")));
                let ids = (0..self.inputs.len())
                    .map(|i| format!("i{i}"))
                    .chain((0..self.outputs.len()).map(|o| format!("o{o}")));
                for (id, cell) in ids.zip(row) {
                    rule.insert(id, json!(Self::prefixed(cell, prefix)));
                }
                Value::Object(rule)
            })
            .collect();
        json!({"hitPolicy": self.hit, "inputs": inputs, "outputs": outputs, "rules": rules})
    }

    fn decision(&self) -> Decision {
        let mut content = self.data("", "");
        if let (Some(content), Some(attributes)) =
            (content.as_object_mut(), self.attributes.as_object())
        {
            content.extend(attributes.clone());
        }
        let graph = json!({
            "nodes": [
                {"id": "in", "name": "in", "type": "inputNode", "content": {}},
                {"id": "t", "name": "t", "type": "decisionTableNode", "content": content},
                {"id": "out", "name": "out", "type": "outputNode", "content": {}}
            ],
            "edges": [
                {"id": "e1", "sourceId": "in", "targetId": "t"},
                {"id": "e2", "sourceId": "t", "targetId": "out"}
            ]
        });
        let DecisionContent::Graph(graph) =
            serde_json::from_value::<DecisionContent>(graph).expect("graph")
        else {
            panic!("not a graph");
        };
        let mut decision = Decision::from(graph);
        decision.compile();
        decision
    }

    fn workspace(&self) -> PolicyWorkspace {
        let properties: Vec<Value> = self
            .models
            .as_object()
            .expect("models")
            .iter()
            .map(|(name, spec)| {
                let ty = spec[0].as_str().unwrap_or("number");
                let (ty, array) = match ty.strip_suffix("[]") {
                    Some(base) => (base, true),
                    None => (ty, false),
                };
                json!({"id": format!("x.{name}"), "name": name, "type": ty, "array": array, "optional": spec[1].as_bool().unwrap_or(true)})
            })
            .collect();
        let doc = match &self.policy {
            Some((doc, _)) => doc.clone(),
            None => json!({"blocks": [
                {"id": "dm", "type": "dataModel", "props": {"data": {"name": "x", "properties": properties}}},
                {"id": "t", "type": "decisionTable", "props": {"data": self.data("x.", "x.")}}
            ]}),
        };
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("p", serde_json::from_value(doc).expect("policy"));
        ws
    }

    fn block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    fn graph_result(
        result: Result<zen_engine::DecisionGraphResponse, Box<zen_engine::EvaluationError>>,
    ) -> String {
        match result {
            Ok(response) => response.result.to_string(),
            Err(error) => format!(
                "ERROR {}",
                Self::error_kind(&serde_json::to_value(&*error).unwrap_or(Value::Null))
            ),
        }
    }

    fn error_kind(value: &Value) -> String {
        value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    }

    fn policy_result(
        result: Result<zen_engine::policy::EvaluationResult, zen_engine::policy::EvaluationError>,
    ) -> (String, String) {
        match result {
            Ok(result) => {
                let trace = result
                    .trace
                    .as_ref()
                    .and_then(|t| t.as_legacy())
                    .and_then(|t| t.executions.iter().find(|e| e.block_id.as_ref() == "t"))
                    .map(|e| {
                        serde_json::to_value(&e.trace)
                            .unwrap_or(Value::Null)
                            .to_string()
                    })
                    .unwrap_or_default();
                let output = result.output.dot("x").unwrap_or(result.output);
                (output.to_string(), trace)
            }
            Err(error) => {
                let debug = format!("{error:?}");
                let kind = debug
                    .split(|c: char| !c.is_alphanumeric())
                    .find(|s| !s.is_empty())
                    .unwrap_or("?")
                    .to_string();
                let inner = debug
                    .split("source: ")
                    .nth(1)
                    .map(|s| {
                        s.split(|c: char| !c.is_alphanumeric())
                            .find(|s| !s.is_empty())
                            .unwrap_or("")
                    })
                    .unwrap_or("");
                (
                    format!(
                        "ERROR {kind}{}",
                        if inner.is_empty() {
                            String::new()
                        } else {
                            format!("/{inner}")
                        }
                    ),
                    String::new(),
                )
            }
        }
    }

    fn run(&self, input: Value) -> Seen {
        let decision = self.decision();
        let context = Variable::from(input.clone());
        let single = Self::graph_result(Self::block(decision.evaluate(context.depth_clone(64))));
        let batch = Self::block(
            decision.evaluate_batch(&[context.depth_clone(64)], EvaluationOptions::default()),
        )
        .into_iter()
        .map(Self::graph_result)
        .next()
        .unwrap_or_default();
        let traced = Self::block(decision.evaluate_with_opts(
            context.depth_clone(64),
            EvaluationOptions {
                trace: zen_engine::trace::TraceFormat::Legacy,
                max_depth: 10,
                ..Default::default()
            },
        ));
        let graph_trace = match &traced {
            Ok(response) => response
                .trace
                .as_ref()
                .and_then(|t| t.as_graph())
                .and_then(|t| t.get("t"))
                .map(|t| {
                    format!(
                        "output={} data={}",
                        t.output,
                        t.trace_data.clone().unwrap_or(Variable::Null)
                    )
                })
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
        let mut graph = vec![
            ("single", single),
            ("batch", batch),
            ("traced", Self::graph_result(traced)),
        ];
        graph.push((
            "walker",
            Self::graph_result(Self::block(
                decision.interpreted().evaluate(context.depth_clone(64)),
            )),
        ));

        let ws = self.workspace();
        let policy_input = match &self.policy {
            Some((_, input)) => input.clone(),
            None => json!({"x": input}),
        };
        let request = |trace: bool| EvaluateRequest {
            policy_path: Arc::from("p"),
            input: Variable::from(policy_input.clone()),
            goals: Vec::new(),
            trace: match trace {
                true => zen_engine::trace::TraceFormat::Legacy,
                false => zen_engine::trace::TraceFormat::Off,
            },
        };
        let (compiled, _) = Self::policy_result(ws.evaluate(&request(false)));
        let (traced, policy_trace) = Self::policy_result(ws.evaluate(&request(true)));
        let batch = Self::policy_result(ws.evaluate_batch(&[request(false)]).remove(0)).0;
        let mut policy = vec![("compiled", compiled), ("batch", batch), ("traced", traced)];
        policy.push((
            "driver",
            Self::policy_result(ws.evaluate_with_driver(&request(false))).0,
        ));
        let rows = vec![policy_input.clone(); 2];
        let built = Built::new(&rows);
        let columns = built.as_ref().map(|built| built.columns());
        let columns = columns
            .as_ref()
            .and_then(|columns| ws.evaluate_columns(&Arc::from("p"), &[], columns).ok())
            .map(|mut output| {
                let text = match output.errors[0].take().map(std::rc::Rc::try_unwrap) {
                    Some(Ok(zen_engine::EvaluationError::Policy(error))) => {
                        Self::policy_result(Err(error)).0
                    }
                    Some(other) => format!("{other:?}"),
                    None => output.row(0).dot("x").unwrap_or(output.row(0)).to_string(),
                };
                (text, output.paths().hosted)
            });
        Seen {
            graph,
            policy,
            graph_trace,
            policy_trace,
            columns,
        }
    }
}

#[test]
fn d01_first_hit_empty_output_cell_is_not_filled_by_later_rules_in_graph() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "b"],
        &[&["> 5", "'a0'", ""], &["> 0", "'a1'", "'b1'"]],
    );
    table
        .run(json!({"n": 6}))
        .columnar()
        .expect(r#"{"a":"a0"}"#, r#"{"n":6,"a":"a0","b":"b1"}"#);
}

#[test]
fn d02_first_hit_no_match() {
    let table = Table::new("first", &[Some("n")], &["a"], &[&["> 5", "1"]]);
    table
        .run(json!({"n": 1}))
        .expect(r#"{}"#, r#"{"n":1,"a":null}"#);
}

#[test]
fn d02b_first_hit_no_match_with_output_path() {
    let table = Table::new("first", &[Some("n")], &["a"], &[&["> 5", "1"]])
        .with_attributes(json!({"outputPath": "res"}));
    table
        .run(json!({"n": 1}))
        .expect(r#"{}"#, r#"{"n":1,"a":null}"#);
}

#[test]
fn d03_collect_hit_policy_shape() {
    let table = Table::new(
        "collect",
        &[Some("n")],
        &["a", "b"],
        &[
            &["> 5", "1", "'x'"],
            &["> 0", "2", ""],
            &["< 0", "3", "'z'"],
        ],
    );
    table.run(json!({"n": 6})).columnar().expect(
        r#"[{"a":1,"b":"x"},{"a":2}]"#,
        r#"{"n":6,"a":[1,2],"b":["x"]}"#,
    );
}

#[test]
fn d04_collect_hit_policy_no_match() {
    let table = Table::new("collect", &[Some("n")], &["a"], &[&["> 5", "1"]]);
    table
        .run(json!({"n": 1}))
        .columnar()
        .expect(r#"[]"#, r#"{"n":1,"a":[]}"#);
}

#[test]
fn d05_column_collect_in_first_table() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "b", "tags[]"],
        &[
            &["> 5", "'a0'", "", "'t0'"],
            &["> 0", "'a1'", "'b1'", "'t1'"],
        ],
    );
    table.run(json!({"n": 6})).columnar().expect(
        r#"{"a":"a0","tags":["t0","t1"]}"#,
        r#"{"n":6,"a":"a0","b":"b1","tags":["t0","t1"]}"#,
    );
}

#[test]
fn d06_column_collect_no_match() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "tags[]"],
        &[&["> 5", "'a0'", "'t0'"]],
    );
    table
        .run(json!({"n": 1}))
        .released(Some("{}"), None)
        .columnar()
        .expect(r#"{"tags":[]}"#, r#"{"n":1,"a":null,"tags":[]}"#);
}

#[test]
fn d07_column_suffix_under_collect_hit_policy() {
    let table = Table::new(
        "collect",
        &[Some("n")],
        &["a", "tags[]"],
        &[&["> 5", "1", "'t0'"], &["> 0", "2", "'t1'"]],
    );
    table.run(json!({"n": 6})).columnar().expect(
        r#"[{"a":1,"tags":"t0"},{"a":2,"tags":"t1"}]"#,
        r#"{"n":6,"a":[1,2],"tags":["t0","t1"]}"#,
    );
}

#[test]
fn d08_input_cell_error() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["$ + 1 > 'x'", "1"], &["", "2"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":2}"#, "ERROR ExpressionFailed/VMError");
}

#[test]
fn d09_output_cell_error() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["> 5", "{p}n + 'x' > 1"], &["", "2"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":2}"#, "ERROR ExpressionFailed/VMError");
}

#[test]
fn d10_unary_cell_without_comparison_is_equality() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["$ + 1", "1"], &["", "2"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":2}"#, r#"{"n":6,"a":2}"#);
}

#[test]
fn d11_fieldless_cell_non_boolean() {
    let table = Table::new("first", &[None], &["a"], &[&["{p}n + 1", "1"], &["", "2"]]);
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":2}"#, r#"{"n":6,"a":2}"#);
}

#[test]
fn d12_dollar_in_output_cell() {
    let table = Table::new("first", &[Some("n")], &["a"], &[&["> 0", "$"]]);
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":6}"#, "ERROR MissingRequiredInputs");
}

#[test]
fn d13_null_and_absent_input() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["null", "'null'"], &["", "'any'"]],
    );
    table
        .run(json!({"n": null}))
        .expect(r#"{"a":"null"}"#, r#"{"n":null,"a":"null"}"#);
    table
        .run(json!({}))
        .expect(r#"{"a":"null"}"#, r#"{"a":"null"}"#);
}

#[test]
fn d14_required_absent_input() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["null", "'null'"], &["", "'any'"]],
    )
    .with_models(json!({"n": ["number", false]}));
    table
        .run(json!({}))
        .expect(r#"{"a":"null"}"#, "ERROR MissingRequiredInputs");
}

#[test]
fn d15_nested_output_paths_and_order() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["r.b", "r.a", "s"],
        &[&["> 5", "", "1", "2"], &["> 0", "3", "4", "5"]],
    );
    table.run(json!({"n": 6})).expect(
        r#"{"r":{"a":1},"s":2}"#,
        r#"{"n":6,"r":{"a":1,"b":3},"s":2}"#,
    );
}

#[test]
fn d16_scalar_parent_then_nested_child() {
    let table = Table::new("first", &[Some("n")], &["r", "r.a"], &[&["> 0", "1", "2"]]);
    table
        .run(json!({"n": 6}))
        .expect(r#"{"r":1}"#, r#"{"n":6,"r":1}"#);
}

#[test]
fn d17_two_collect_columns_one_field() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["v[]", "v[]"],
        &[&["> 0", "1", "'a'"], &["> 0", "2", "'b'"]],
    );
    table
        .run(json!({"n": 6}))
        .released(Some(r#"{"v":["a","b"]}"#), None)
        .columnar()
        .expect(r#"{"v":[1,"a",2,"b"]}"#, r#"{"n":6,"v":[1,"a",2,"b"]}"#);
}

#[test]
fn d18_duplicate_scalar_columns() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["v", "v"],
        &[&["> 5", "", "1"], &["> 0", "2", "3"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"v":1}"#, r#"{"n":6,"v":2}"#);
}

#[test]
fn d19_whitespace_cell() {
    let table = Table::new("first", &[Some("n")], &["a"], &[&["  ", "1"], &["", "2"]]);
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":1}"#, "ERROR ExpressionFailed/ParserError");
}

#[test]
fn d20_unparsable_cell() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["> > 1", "1"], &["", "2"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":2}"#, "ERROR ExpressionFailed/ParserError");
}

#[test]
fn d21_number_scale() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "b"],
        &[&["1.0", "1.10", "{p}n * 1.00"]],
    );
    table
        .run(json!({"n": 1}))
        .expect(r#"{"a":1.10,"b":1.00}"#, r#"{"n":1,"a":1.10,"b":1.00}"#);
}

#[test]
fn d22_list_valued_input() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[
            &["'a'", "'eq'"],
            &["contains($, 'b')", "'has b'"],
            &["", "'other'"],
        ],
    )
    .with_models(json!({"n": ["string[]", true]}));
    table
        .run(json!({"n": ["a", "b"]}))
        .expect(r#"{"a":"has b"}"#, r#"{"n":["a","b"],"a":"has b"}"#);
}

#[test]
fn d23_date_input_coercion() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["> d('2024-01-01')", "'after'"], &["", "'other'"]],
    )
    .with_models(json!({"n": ["date", true]}));
    table
        .run(json!({"n": "2025-01-01"}))
        .expect(r#"{"a":"after"}"#, r#"{"n":2025-01-01,"a":"after"}"#);
}

#[test]
fn d24_rule_with_no_outputs() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["> 5", ""], &["> 0", "1"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{}"#, r#"{"n":6,"a":1}"#);
}

#[test]
fn d25_pass_through_no_match() {
    let table = Table::new("first", &[Some("n")], &["a"], &[&["> 5", "1"]])
        .with_attributes(json!({"passThrough": true}));
    table
        .run(json!({"n": 1, "a": 9}))
        .expect(r#"{"a":9,"n":1}"#, r#"{"a":null,"n":1}"#);
}

#[test]
fn d26_loop_mode_vs_member_table() {
    let doc = json!({"blocks": [
        {"id": "dm", "type": "dataModel", "props": {"data": {"name": "c", "properties": [
            {"id": "c.items", "name": "items", "type": "relationship", "target": "item", "array": true, "optional": false}
        ]}}},
        {"id": "di", "type": "dataModel", "props": {"data": {"name": "item", "properties": [
            {"id": "item.n", "name": "n", "type": "number", "array": false, "optional": false}
        ]}}},
        {"id": "t", "type": "decisionTable", "props": {"data": Table::new("first", &[Some("item.n")], &["item.a"], &[&["> 5", "1"]]).data("", "")}}
    ]});
    let table = Table::new("first", &[Some("n")], &["a"], &[&["> 5", "1"]])
        .with_attributes(json!({"executionMode": "loop", "inputField": "items", "outputPath": "items", "passThrough": true}))
        .with_policy(doc, json!({"c": {"items": [{"n": 6}, {"n": 1}]}}));
    table.run(json!({"items": [{"n": 6}, {"n": 1}]})).expect(
        r#"{"items":[{"n":6,"a":1},{"n":1}]}"#,
        r#"{"c":{"items":[{"n":6,"a":1},{"n":1,"a":null}]}}"#,
    );
}

#[test]
fn d27_input_field_expression_error() {
    let table = Table::new(
        "first",
        &[Some("n + 'x' > 1")],
        &["a"],
        &[&["true", "1"], &["", "2"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":2}"#, "ERROR ExpressionFailed/VMError");
}

#[test]
fn d28_collect_output_error() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "v[]"],
        &[&["> 0", "1", "{p}n + 'x' > 1"], &["> 0", "2", "'ok'"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":2,"v":["ok"]}"#, "ERROR ExpressionFailed/VMError");
}

#[test]
fn d29_no_rules() {
    let table = Table::new("first", &[Some("n")], &["a", "v[]"], &[]);
    table
        .run(json!({"n": 6}))
        .released(Some("{}"), None)
        .expect(r#"{"v":[]}"#, r#"{"n":6,"a":null,"v":[]}"#);
}

#[test]
fn d30_existing_input_value_at_output_path() {
    let table = Table::new("first", &[Some("n")], &["a"], &[&["> 5", "1"]]);
    table
        .run(json!({"n": 1, "a": 9}))
        .expect(r#"{}"#, r#"{"a":null,"n":1}"#);
}

#[test]
fn d31_write_order_of_outputs_filled_by_different_rules() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "b"],
        &[&["> 5", "", "'b0'"], &["> 0", "'a1'", "'b1'"]],
    );
    table
        .run(json!({"n": 6}))
        .columnar()
        .expect(r#"{"b":"b0"}"#, r#"{"n":6,"b":"b0","a":"a1"}"#);
}

#[test]
fn d32_policy_keeps_scanning_rows_after_the_first_match() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "b"],
        &[
            &["> 5", "'a0'", ""],
            &["$ + 'x' > 1", "'a1'", "'b1'"],
            &["", "", "'b2'"],
        ],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":"a0"}"#, "ERROR ExpressionFailed/VMError");
}

#[test]
fn d33_policy_stops_once_every_scalar_is_resolved() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a"],
        &[&["> 5", "'a0'"], &["$ + 'x' > 1", "'a1'"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":"a0"}"#, r#"{"n":6,"a":"a0"}"#);
}

#[test]
fn d34_collect_values_with_empty_cells_and_nulls() {
    let table = Table::new(
        "collect",
        &[Some("n")],
        &["a", "b"],
        &[&["> 0", "null", ""], &["> 0", "", "1"]],
    );
    table
        .run(json!({"n": 6}))
        .released(None, Some(r#"{"n":6,"b":[1],"a":[null]}"#))
        .columnar()
        .expect(r#"[{"a":null},{"b":1}]"#, r#"{"n":6,"a":[null],"b":[1]}"#);
}

#[test]
fn d35_first_collect_first_rule_scalar_error_drops_the_rule() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a", "v[]"],
        &[&["> 0", "{p}n + 'x' > 1", "1"], &["> 0", "'a1'", "2"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a":"a1","v":[2]}"#, "ERROR ExpressionFailed/VMError");
}

#[test]
fn d36_first_collect_only_collect_columns_and_no_match() {
    let table = Table::new("first", &[Some("n")], &["v[]"], &[&["> 5", "1"]])
        .with_attributes(json!({"outputPath": "res"}));
    table
        .run(json!({"n": 1}))
        .released(Some("{}"), None)
        .expect(r#"{"res":{"v":[]}}"#, r#"{"n":1,"v":[]}"#);
}

#[test]
fn d37_collect_marker_inside_a_path() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["a[].b", "c"],
        &[&["> 0", "1", "2"]],
    );
    table
        .run(json!({"n": 6}))
        .expect(r#"{"a[]":{"b":1},"c":2}"#, r#"{"n":6,"c":2}"#);
}

#[test]
fn d38_dotted_collect_path_and_scalar_sibling() {
    let table = Table::new(
        "first",
        &[Some("n")],
        &["r.s", "r.v[]"],
        &[&["> 5", "'big'", "1"], &["> 0", "'small'", "2"]],
    );
    table.run(json!({"n": 6})).columnar().expect(
        r#"{"r":{"s":"big","v":[1,2]}}"#,
        r#"{"n":6,"r":{"s":"big","v":[1,2]}}"#,
    );
}

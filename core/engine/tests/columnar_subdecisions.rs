use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use zen_engine::loader::MemoryLoader;
use zen_engine::model::{DecisionContent, GraphContent};
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::nodes::{NodeError, NodeResponse, NodeResult};
use zen_engine::{Decision, EvaluationOptions};
use zen_expression::lane::{Column, Columns, Values};
use zen_expression::Variable;

#[derive(Debug, Default, Clone)]
struct Counts(Arc<Mutex<BTreeMap<String, usize>>>);

impl Counts {
    fn bump(&self, key: &str) {
        if let Ok(mut map) = self.0.lock() {
            *map.entry(key.to_string()).or_default() += 1;
        }
    }

    fn take(&self) -> BTreeMap<String, usize> {
        self.0
            .lock()
            .map(|mut map| std::mem::take(&mut *map))
            .unwrap_or_default()
    }
}

#[derive(Debug)]
struct Adapter(Counts);

impl CustomNodeAdapter for Adapter {
    fn handle(&self, request: CustomNodeRequest) -> Pin<Box<dyn Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            self.0.bump(&request.node.name);
            let odd = request
                .input
                .dot("x")
                .is_some_and(|x| x.to_value().as_i64().is_some_and(|x| x % 2 == 1));
            let output = match request.node.kind.as_ref() {
                "array" => Variable::from(json!([1, 2])),
                "flaky" if odd => {
                    return Err(NodeError {
                        node_id: request.node.id.clone(),
                        trace: None,
                        source: "odd row".into(),
                    })
                }
                "echo" | "flaky" => request.input.clone(),
                _ => Variable::from(json!({ "seen": true })),
            };
            Ok(NodeResponse {
                output,
                trace_data: None,
            })
        })
    }
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn input(name: &str) -> Value {
    json!({ "id": name, "name": name, "type": "inputNode" })
}

fn output() -> Value {
    json!({ "id": "out", "name": "out", "type": "outputNode" })
}

fn expression(id: &str, fields: &[(&str, &str)]) -> Value {
    let expressions: Vec<Value> = fields
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({ "id": format!("{id}-{i}"), "key": key, "value": value }))
        .collect();
    node(id, "expressionNode", json!({ "expressions": expressions }))
}

fn decision(id: &str, key: &str, attributes: Value) -> Value {
    let mut content = json!({ "key": key });
    if let (Some(map), Some(extra)) = (content.as_object_mut(), attributes.as_object()) {
        map.extend(extra.clone());
    }
    node(id, "decisionNode", content)
}

fn custom(id: &str, kind: &str) -> Value {
    node(id, "customNode", json!({ "kind": kind, "config": {} }))
}

fn chain(nodes: Vec<Value>) -> Value {
    let ids: Vec<String> = nodes
        .iter()
        .map(|n| n["id"].as_str().unwrap_or_default().to_string())
        .collect();
    let edges: Vec<Value> = ids
        .windows(2)
        .map(|w| json!({ "id": format!("{}-{}", w[0], w[1]), "sourceId": w[0], "targetId": w[1] }))
        .collect();
    json!({ "nodes": nodes, "edges": edges })
}

struct Scenario {
    name: &'static str,
    graph: Value,
    children: Vec<(&'static str, Value)>,
    planned: bool,
    max_depth: u8,
}

impl Scenario {
    const ROWS: i64 = 6;

    fn all() -> Vec<Scenario> {
        let child = chain(vec![
            input("childIn"),
            expression("c", &[("double", "x * 2"), ("label", "'n' + string(x)")]),
            output(),
        ]);
        vec![
            Scenario {
                name: "plain child",
                graph: chain(vec![
                    input("in"),
                    decision("call", "child", json!({})),
                    output(),
                ]),
                children: vec![("child", child.clone())],
                planned: true,
                max_depth: 10,
            },
            Scenario {
                name: "output path and pass through",
                graph: chain(vec![
                    input("in"),
                    decision(
                        "call",
                        "child",
                        json!({ "transformAttributes": { "outputPath": "res.inner", "passThrough": true } }),
                    ),
                    expression("after", &[("total", "res.inner.double + x")]),
                    output(),
                ]),
                children: vec![("child", child.clone())],
                planned: true,
                max_depth: 10,
            },
            Scenario {
                name: "input field",
                graph: chain(vec![
                    input("in"),
                    expression("wrap", &[("inner.x", "x + 1")]),
                    decision(
                        "call",
                        "child",
                        json!({ "transformAttributes": { "inputField": "inner" } }),
                    ),
                    output(),
                ]),
                children: vec![("child", child.clone())],
                planned: true,
                max_depth: 10,
            },
            Scenario {
                name: "grandchild reads parents through input views",
                graph: chain(vec![
                    input("root"),
                    expression("pre", &[("r", "x * 10"), ("x", "x")]),
                    decision("call", "mid", json!({})),
                    output(),
                ]),
                children: vec![
                    (
                        "mid",
                        chain(vec![
                            input("midIn"),
                            expression("m", &[("x", "x"), ("mid", "x + 1")]),
                            decision("call", "leaf", json!({})),
                            output(),
                        ]),
                    ),
                    (
                        "leaf",
                        chain(vec![
                            input("leafIn"),
                            custom("probe", "echo"),
                            expression(
                                "l",
                                &[
                                    ("fromMid", "$nodes.leafIn.$nodes.m.mid"),
                                    ("fromRoot", "$nodes.leafIn.$nodes.midIn.$nodes.pre.r"),
                                ],
                            ),
                            output(),
                        ]),
                    ),
                ],
                planned: false,
                max_depth: 10,
            },
            Scenario {
                name: "child errors on odd rows",
                graph: chain(vec![
                    input("in"),
                    decision(
                        "call",
                        "flaky",
                        json!({ "transformAttributes": { "outputPath": "res" } }),
                    ),
                    output(),
                ]),
                children: vec![(
                    "flaky",
                    chain(vec![input("flakyIn"), custom("side", "flaky"), output()]),
                )],
                planned: true,
                max_depth: 10,
            },
            Scenario {
                name: "child side effect then parent array",
                graph: chain(vec![
                    input("in"),
                    decision("call", "effect", json!({})),
                    custom("arr", "array"),
                    expression("e", &[("y", "1")]),
                    output(),
                ]),
                children: vec![(
                    "effect",
                    chain(vec![input("effectIn"), custom("inner", "object"), output()]),
                )],
                planned: false,
                max_depth: 10,
            },
            Scenario {
                name: "depth limit",
                graph: chain(vec![
                    input("in"),
                    decision("call", "a", json!({})),
                    output(),
                ]),
                children: vec![
                    (
                        "a",
                        chain(vec![
                            input("aIn"),
                            decision("next", "b", json!({})),
                            output(),
                        ]),
                    ),
                    (
                        "b",
                        chain(vec![
                            input("bIn"),
                            decision("next", "c", json!({})),
                            output(),
                        ]),
                    ),
                    (
                        "c",
                        chain(vec![
                            input("cIn"),
                            expression("e", &[("deep", "x")]),
                            output(),
                        ]),
                    ),
                ],
                planned: false,
                max_depth: 3,
            },
        ]
    }

    fn decision(&self, counts: &Counts) -> Decision {
        let memory = MemoryLoader::default();
        for (key, graph) in &self.children {
            let content: GraphContent = serde_json::from_value(graph.clone()).expect("child");
            memory.add(*key, DecisionContent::Graph(Arc::new(content)));
        }
        Decision::from(Arc::new(
            serde_json::from_value::<GraphContent>(self.graph.clone()).expect("graph"),
        ))
        .with_adapter(Arc::new(Adapter(counts.clone())))
        .with_loader(Arc::new(memory))
        .compiled_engine()
    }

    fn error(error: &zen_engine::EvaluationError) -> String {
        serde_json::to_value(error)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| error.to_string())
    }

    async fn check(&self) -> Vec<String> {
        let counts = Counts::default();
        let decision = self.decision(&counts);
        let options = EvaluationOptions {
            trace: false,
            max_depth: self.max_depth,
            ..Default::default()
        };
        let xs: Vec<i64> = (1..=Self::ROWS).collect();
        let columns = Columns::new(xs.len()).column("x", Column::new(Values::I64(&xs)));
        let contexts: Vec<Variable> = xs
            .iter()
            .map(|x| Variable::from(json!({ "x": x })))
            .collect();
        let mut failures = Vec::new();
        let columnar = decision.evaluate_columns(&columns, options).await;
        let columnar_counts = counts.take();
        if self.planned {
            match zen_engine::diagnostics::plan_verdict(&decision, &columns) {
                Some(Ok(())) => {}
                other => failures.push(format!("{}: not planned: {other:?}", self.name)),
            }
        }
        let mut singles = Vec::new();
        for context in &contexts {
            singles.push(decision.evaluate_with_opts(context.clone(), options).await);
        }
        let single_counts = counts.take();
        let batch = decision.evaluate_batch(&contexts, options).await;
        for (row, (single, batched)) in singles.iter().zip(&batch).enumerate() {
            let want = single
                .as_ref()
                .map(|r| r.result.to_value())
                .map_err(|e| Self::error(e));
            let other = batched
                .as_ref()
                .map(|r| r.result.to_value())
                .map_err(|e| Self::error(e));
            let got = match &columnar.errors[row] {
                Some(error) => Err(Self::error(error)),
                None => Ok(columnar.row(row).to_value()),
            };
            if got != want {
                failures.push(format!(
                    "{} row {row}: columnar {got:?} vs single {want:?}",
                    self.name
                ));
            }
            if other != want {
                failures.push(format!(
                    "{} row {row}: batch {other:?} vs single {want:?}",
                    self.name
                ));
            }
        }
        if columnar_counts != single_counts {
            failures.push(format!(
                "{}: custom calls columnar {columnar_counts:?} vs single {single_counts:?}",
                self.name
            ));
        }
        failures
    }
}

#[tokio::test]
async fn columnar_sub_decisions_match_row_evaluation() {
    let mut failures = Vec::new();
    for scenario in Scenario::all() {
        failures.extend(scenario.check().await);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

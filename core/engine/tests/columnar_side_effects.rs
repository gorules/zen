use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use zen_engine::loader::{DecisionLoader, LoaderResponse, MemoryLoader};
use zen_engine::model::{DecisionContent, GraphContent};
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::nodes::http_handler::{HttpHandler, HttpHandlerRequest, HttpHandlerResponse};
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
struct Counting(Counts);

impl CustomNodeAdapter for Counting {
    fn handle(&self, request: CustomNodeRequest) -> Pin<Box<dyn Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            self.0.bump(&format!("custom:{}", request.node.name));
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
                "flaky" => Variable::from(json!([1, 2])),
                _ => Variable::from(json!({ "seen": true })),
            };
            Ok(NodeResponse {
                output,
                trace_data: None,
            })
        })
    }
}

#[derive(Debug)]
struct Http(Counts);

impl HttpHandler for Http {
    fn handle(
        &self,
        _request: HttpHandlerRequest,
    ) -> Pin<Box<dyn Future<Output = Result<HttpHandlerResponse, String>> + Send + '_>> {
        Box::pin(async move {
            self.0.bump("http");
            Ok(HttpHandlerResponse {
                status: 200,
                headers: json!({}),
                data: json!({ "ok": true }),
            })
        })
    }
}

#[derive(Debug)]
struct Loads(Counts, MemoryLoader);

impl DecisionLoader for Loads {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = LoaderResponse> + 'a + Send>> {
        self.0.bump(&format!("load:{key}"));
        self.1.load(key)
    }
}

struct Scenario {
    name: &'static str,
    graph: Value,
}

impl Scenario {
    const ROWS: usize = 4;

    fn node(id: &str, kind: &str, content: Value) -> Value {
        json!({ "id": id, "name": id, "type": kind, "content": content })
    }

    fn edge(from: &str, to: &str) -> Value {
        json!({ "id": format!("{from}-{to}"), "sourceId": from, "targetId": to })
    }

    fn switched(from: &str, to: &str, handle: &str) -> Value {
        json!({ "id": format!("{from}-{to}"), "sourceId": from, "targetId": to, "sourceHandle": handle })
    }

    fn expression(id: &str, key: &str, value: &str) -> Value {
        Self::node(
            id,
            "expressionNode",
            json!({ "expressions": [{ "id": format!("{id}-1"), "key": key, "value": value }] }),
        )
    }

    fn custom(id: &str, kind: &str) -> Value {
        Self::node(id, "customNode", json!({ "kind": kind, "config": {} }))
    }

    fn io() -> [Value; 2] {
        [
            json!({ "id": "in", "name": "in", "type": "inputNode" }),
            json!({ "id": "out", "name": "out", "type": "outputNode" }),
        ]
    }

    fn all() -> Vec<Scenario> {
        let [input, output] = Self::io();
        let http = Self::node(
            "fetch",
            "functionNode",
            json!({ "source": "import http from 'http';\nexport const handler = async (input) => {\n  const r = await http.get('https://example.invalid/x');\n  return { status: r.status };\n};\n" }),
        );
        let switch = Self::node(
            "sw",
            "switchNode",
            json!({ "hitPolicy": "first", "statements": [{ "id": "hi", "condition": "x > 2" }, { "id": "lo", "condition": "" }] }),
        );
        vec![
            Scenario {
                name: "control: custom returns object",
                graph: json!({
                    "nodes": [input, Self::custom("side", "object"), Self::expression("e", "y", "1"), output],
                    "edges": [Self::edge("in", "side"), Self::edge("side", "e"), Self::edge("e", "out")]
                }),
            },
            Scenario {
                name: "strict join sees array",
                graph: json!({
                    "nodes": [input, Self::custom("side", "array"), Self::expression("e", "y", "1"), output],
                    "edges": [Self::edge("in", "side"), Self::edge("side", "e"), Self::edge("e", "out")]
                }),
            },
            Scenario {
                name: "lazy child plan fails",
                graph: json!({
                    "nodes": [input, Self::custom("side", "object"), Self::expression("e", "z", "1"), switch, output],
                    "edges": [
                        Self::edge("in", "side"), Self::edge("side", "e"), Self::edge("in", "sw"),
                        Self::switched("sw", "out", "hi"), Self::switched("sw", "out", "lo")
                    ]
                }),
            },
            Scenario {
                name: "custom fails on odd rows, array on even rows",
                graph: json!({
                    "nodes": [input, Self::custom("side", "flaky"), Self::expression("e", "y", "1"), output],
                    "edges": [Self::edge("in", "side"), Self::edge("side", "e"), Self::edge("e", "out")]
                }),
            },
            Scenario {
                name: "sub-decision then array",
                graph: json!({
                    "nodes": [input, Self::node("sub", "decisionNode", json!({ "key": "sub" })), Self::custom("side", "array"), Self::expression("e", "y", "1"), output],
                    "edges": [Self::edge("in", "sub"), Self::edge("sub", "side"), Self::edge("side", "e"), Self::edge("e", "out")]
                }),
            },
            Scenario {
                name: "v2 function http then array",
                graph: json!({
                    "nodes": [input, http, Self::custom("side", "array"), Self::expression("e", "y", "1"), output],
                    "edges": [Self::edge("in", "fetch"), Self::edge("fetch", "side"), Self::edge("side", "e"), Self::edge("e", "out")]
                }),
            },
        ]
    }

    fn compiled(value: Value) -> Arc<GraphContent> {
        let mut graph: GraphContent = serde_json::from_value(value).expect("graph");
        graph.compile();
        Arc::new(graph)
    }

    fn decision(&self, counts: &Counts) -> Decision {
        let [input, output] = Self::io();
        let sub = json!({
            "nodes": [input, Self::custom("inner", "object"), output],
            "edges": [Self::edge("in", "inner"), Self::edge("inner", "out")]
        });
        let memory = MemoryLoader::default();
        memory.add("sub", DecisionContent::Graph(Self::compiled(sub)));
        let mut decision = Decision::from(Arc::new(
            serde_json::from_value::<GraphContent>(self.graph.clone()).expect("graph"),
        ))
        .with_adapter(Arc::new(Counting(counts.clone())))
        .with_http_handler(Some(Arc::new(Http(counts.clone()))))
        .with_loader(Arc::new(Loads(counts.clone(), memory)))
        .compiled_engine();
        decision.compile();
        decision
    }

    fn outcome(
        result: &Result<zen_engine::DecisionGraphResponse, Box<zen_engine::EvaluationError>>,
    ) -> Result<Value, String> {
        match result {
            Ok(response) => Ok(response.result.to_value()),
            Err(error) => Err(serde_json::to_value(&**error)
                .map(|v| v.to_string())
                .unwrap_or_else(|_| error.to_string())),
        }
    }

    async fn measure(&self) -> (BTreeMap<&'static str, BTreeMap<String, usize>>, Vec<String>) {
        let counts = Counts::default();
        let decision = self.decision(&counts);
        let xs: Vec<i64> = (1..=Self::ROWS as i64).collect();
        let columns = Columns::new(Self::ROWS)
            .column("x", Column::new(Values::I64(&xs)))
            .column("$nodes.k", Column::new(Values::I64(&xs)));
        let contexts: Vec<Variable> = xs
            .iter()
            .map(|x| Variable::from(json!({ "x": x, "$nodes": { "k": x } })))
            .collect();
        let mut out = BTreeMap::new();
        let columnar = decision
            .evaluate_columns(&columns, EvaluationOptions::default())
            .await;
        out.insert("columns", counts.take());
        let batch = decision
            .evaluate_batch(&contexts, EvaluationOptions::default())
            .await;
        out.insert("batch", counts.take());
        let mut mismatches = Vec::new();
        for (row, result) in batch.iter().enumerate() {
            let want = Self::outcome(result);
            let got = match &columnar.errors[row] {
                Some(error) => Err(serde_json::to_value(&**error)
                    .map(|v| v.to_string())
                    .unwrap_or_else(|_| error.to_string())),
                None => Ok(columnar.row(row).to_value()),
            };
            if got != want {
                mismatches.push(format!(
                    "{} row {row}: columns {got:?} vs batch {want:?}",
                    self.name
                ));
            }
        }
        for context in &contexts {
            let _ = decision.evaluate(context.clone()).await;
        }
        out.insert("single", counts.take());
        {
            let walker = decision.interpreted();
            for context in &contexts {
                let _ = walker.evaluate(context.clone()).await;
            }
            out.insert("walker", counts.take());
        }
        (out, mismatches)
    }
}

#[tokio::test]
async fn columnar_host_nodes_run_once_per_row() {
    let mut failures = Vec::new();
    for scenario in Scenario::all() {
        let (measured, mismatches) = scenario.measure().await;
        println!("{}: {measured:?}", scenario.name);
        failures.extend(mismatches);
        let Some(single) = measured.get("single") else {
            continue;
        };
        let hosted = |counts: &BTreeMap<String, usize>| {
            counts
                .iter()
                .filter(|(k, _)| !k.starts_with("load:"))
                .map(|(k, v)| (k.clone(), *v))
                .collect::<BTreeMap<_, _>>()
        };
        let loads = |counts: &BTreeMap<String, usize>| {
            counts
                .iter()
                .filter(|(k, _)| k.starts_with("load:"))
                .map(|(k, v)| (k.clone(), *v))
                .collect::<BTreeMap<_, _>>()
        };
        if let (Some(columns), Some(batch)) = (measured.get("columns"), measured.get("batch")) {
            if loads(columns) != loads(batch) {
                failures.push(format!(
                    "{} columns loads {:?} vs batch {:?}",
                    scenario.name,
                    loads(columns),
                    loads(batch)
                ));
            }
        }
        for (mode, counts) in &measured {
            if hosted(counts) != hosted(single) {
                failures.push(format!(
                    "{} {mode} {counts:?} vs single {single:?}",
                    scenario.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

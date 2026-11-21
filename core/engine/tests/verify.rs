#[allow(dead_code)]
mod compiled_default;
mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use zen_engine::model::GraphContent;
use zen_engine::policy::{EvaluateRequest, EvaluationResult, PolicyWorkspace};
use zen_engine::{Decision, DecisionGraphResponse, EvaluationError, EvaluationOptions};
use zen_expression::Variable;

struct Graphs;

impl Graphs {
    fn chain(nodes: Vec<Value>) -> Value {
        let mut all = vec![json!({ "id": "in", "name": "in", "type": "inputNode" })];
        all.extend(nodes);
        all.push(json!({ "id": "out", "name": "out", "type": "outputNode" }));
        let edges: Vec<Value> = all
            .windows(2)
            .enumerate()
            .map(|(i, w)| json!({ "id": format!("e{i}"), "type": "edge", "sourceId": w[0]["id"], "targetId": w[1]["id"] }))
            .collect();
        json!({ "nodes": all, "edges": edges })
    }

    fn expression(id: &str, pairs: &[(&str, &str)]) -> Value {
        let expressions: Vec<Value> = pairs
            .iter()
            .enumerate()
            .map(|(i, (key, value))| json!({ "id": format!("{id}-{i}"), "key": key, "value": value }))
            .collect();
        json!({ "id": id, "name": id, "type": "expressionNode", "content": { "expressions": expressions, "passThrough": true } })
    }

    fn table(id: &str, outputs: &[(&str, &str)]) -> Value {
        let columns: Vec<Value> = outputs
            .iter()
            .enumerate()
            .map(|(i, (field, _))| json!({ "id": format!("o{i}"), "name": field, "field": field, "type": "expression" }))
            .collect();
        let mut rule = serde_json::Map::new();
        rule.insert("_id".into(), json!("r1"));
        rule.insert("i1".into(), json!(""));
        for (i, (_, cell)) in outputs.iter().enumerate() {
            rule.insert(format!("o{i}"), json!(cell));
        }
        json!({ "id": id, "name": id, "type": "decisionTableNode", "content": {
            "hitPolicy": "first",
            "inputs": [ { "id": "i1", "name": "x", "field": "x", "type": "expression" } ],
            "outputs": columns,
            "rules": [ Value::Object(rule) ],
            "passThrough": true
        } })
    }

    fn content(graph: &Value) -> GraphContent {
        serde_json::from_value(graph.clone()).unwrap()
    }

    fn decision(graph: &Value) -> Decision {
        Decision::from(Arc::new(Self::content(graph)))
    }

    fn text(result: Result<DecisionGraphResponse, Box<EvaluationError>>) -> String {
        match result {
            Ok(response) => serde_json::to_string(&response.result).unwrap(),
            Err(error) => format!("ERR {error}"),
        }
    }

    fn unchanged(first: &str, second: &str) -> Vec<String> {
        let fields = |text: &str| match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(map)) => map.into_iter().filter(|(key, _)| key != "x").collect(),
            _ => serde_json::Map::from_iter([("?".to_string(), Value::from(text))]),
        };
        let second = fields(second);
        fields(first)
            .into_iter()
            .filter(|(key, value)| second.get(key) == Some(value))
            .map(|(key, _)| key)
            .collect()
    }
}

struct Stripped;

impl Stripped {
    const SOURCE: &'static str =
        "export const handler = (input: { x: number }) => ({ via: 'strip' });";
    const CACHED: &'static str = "export const handler = (input) => ({ via: 'cache' });";

    fn graph() -> Value {
        Graphs::chain(vec![
            json!({ "id": "f", "name": "f", "type": "functionNode", "content": { "source": Self::SOURCE } }),
        ])
    }

    fn primed() -> GraphContent {
        let mut content = Graphs::content(&Self::graph());
        content.compile();
        let stripped: ahash::HashMap<Arc<str>, Arc<str>> =
            [(Arc::from(Self::SOURCE), Arc::from(Self::CACHED))]
                .into_iter()
                .collect();
        content.stripped_functions = Some(Arc::new(stripped));
        content
    }
}

struct Policies;

impl Policies {
    fn workspace(doc: Value) -> PolicyWorkspace {
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("p", serde_json::from_value(doc).unwrap());
        ws
    }

    fn request(input: &Value, trace: bool) -> EvaluateRequest {
        EvaluateRequest {
            policy_path: Arc::from("p"),
            input: Variable::from(input.clone()),
            goals: Vec::new(),
            trace: match trace {
                true => zen_engine::trace::TraceFormat::Legacy,
                false => zen_engine::trace::TraceFormat::Off,
            },
        }
    }

    fn text(result: Result<EvaluationResult, zen_engine::policy::EvaluationError>) -> String {
        match result {
            Ok(result) => serde_json::to_string(&result.output).unwrap(),
            Err(error) => format!("ERR {error:?}"),
        }
    }

    fn columnar(
        ws: &PolicyWorkspace,
        columns: &zen_expression::lane::Columns,
        rows: usize,
    ) -> Vec<String> {
        let output = ws.evaluate_columns(&Arc::from("p"), &[], columns).unwrap();
        (0..rows)
            .map(|row| match &output.errors[row] {
                Some(error) => format!("ERR {error:?}"),
                None => serde_json::to_string(&output.row(row)).unwrap(),
            })
            .collect()
    }

    fn mismatches(label: &str, doc: Value, inputs: &[Value]) -> Vec<String> {
        let ws = Self::workspace(doc);
        let driver: Vec<String> = inputs
            .iter()
            .map(|input| Self::text(ws.evaluate_with_driver(&Self::request(input, false))))
            .collect();
        let single = inputs
            .iter()
            .map(|input| Self::text(ws.evaluate(&Self::request(input, false))))
            .collect();
        let requests: Vec<EvaluateRequest> = inputs
            .iter()
            .map(|input| Self::request(input, false))
            .collect();
        let batch = ws
            .evaluate_batch(&requests)
            .into_iter()
            .map(Self::text)
            .collect();
        let columns = match Built::nested(inputs) {
            Some(built) => {
                let fields = built.fields();
                let structs = built.structs(&fields);
                Self::columnar(&ws, &built.columns_with(&structs), inputs.len())
            }
            None => Self::columnar(&ws, &Built::new(inputs).unwrap().columns(), inputs.len()),
        };
        [("single", single), ("batch", batch), ("columns", columns)]
            .into_iter()
            .flat_map(|(path, got): (&str, Vec<String>)| {
                driver
                    .iter()
                    .zip(got)
                    .enumerate()
                    .filter(|(_, (want, got))| *want != got)
                    .map(move |(row, (want, got))| {
                        format!("{label} {path} row {row}:\n  driver {want}\n  {path} {got}")
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

#[tokio::test]
async fn compiled_table_cells_reread_the_clock() {
    assert!(std::env::var("__ZEN_MOCK_UTC_TIME").is_err());
    let graph = Graphs::chain(vec![
        Graphs::expression(
            "ex",
            &[
                ("second", "startOf('now', 'second')"),
                ("end", "endOf('now', 'second')"),
            ],
        ),
        Graphs::table(
            "dt",
            &[
                ("cell", "startOf('now', 'second')"),
                ("cellEnd", "endOf('now', 'second')"),
            ],
        ),
    ]);
    let input = json!({ "x": 1 });
    let walker = Graphs::decision(&graph).interpreted();
    let single = Graphs::decision(&graph);
    let batch = Graphs::decision(&graph);
    let mut compiled = Graphs::decision(&graph);
    compiled.compile();
    let contexts = [Variable::from(input.clone()), Variable::from(input.clone())];
    let built = Built::new(&[input.clone(), input.clone()]).unwrap();
    let columns = built.columns();
    let first = |results: Vec<Result<DecisionGraphResponse, Box<EvaluationError>>>| {
        results.into_iter().map(Graphs::text).next().unwrap()
    };
    let run = || async {
        let columnar = compiled
            .evaluate_columns(&columns, EvaluationOptions::default())
            .await;
        [
            (
                "walker",
                Graphs::text(walker.evaluate(Variable::from(input.clone())).await),
            ),
            (
                "single",
                Graphs::text(single.evaluate(Variable::from(input.clone())).await),
            ),
            (
                "batch",
                first(
                    batch
                        .evaluate_batch(&contexts, EvaluationOptions::default())
                        .await,
                ),
            ),
            (
                "compiled-single",
                Graphs::text(compiled.evaluate(Variable::from(input.clone())).await),
            ),
            (
                "compiled-batch",
                first(
                    compiled
                        .evaluate_batch(&contexts, EvaluationOptions::default())
                        .await,
                ),
            ),
            ("columnar", serde_json::to_string(&columnar.row(0)).unwrap()),
        ]
    };
    let before = run().await;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let after = run().await;
    let stale: Vec<String> = before
        .iter()
        .zip(&after)
        .filter_map(|((path, a), (_, b))| {
            let unchanged = Graphs::unchanged(a, b);
            (!unchanged.is_empty()).then(|| format!("{path}: {unchanged:?}"))
        })
        .collect();
    assert!(stale.is_empty(), "values frozen at compile time: {stale:?}");
}

#[test]
fn policy_columnar_keeps_negative_zero() {
    let doc = json!({ "blocks": [
        { "id": "dm", "type": "dataModel", "props": { "data": {
            "name": "order",
            "properties": [
                { "id": "p1", "name": "amount", "type": "number", "array": false, "optional": false },
                { "id": "p2", "name": "items", "type": "number", "array": true, "optional": false }
            ]
        } } },
        { "id": "e1", "type": "expression", "props": { "data": { "key": "order.neg", "value": "-order.amount" } } },
        { "id": "e2", "type": "expression", "props": { "data": { "key": "order.list", "value": "map(order.items, -#)" } } },
        { "id": "m", "type": "match", "props": { "data": { "key": "order.matched", "arms": [
            { "id": "a1", "condition": "order.amount > 5", "value": "1" },
            { "id": "a2", "condition": "", "value": "-0" }
        ] } } },
        { "id": "dt", "type": "decisionTable", "props": { "data": {
            "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
            "outputs": [ { "id": "o1", "name": "", "field": "order.tneg" }, { "id": "o2", "name": "", "field": "order.tags[]" } ],
            "rules": [ { "i1": "", "o1": "-order.amount", "o2": "-order.amount" } ]
        } } },
        { "id": "s1", "type": "expression", "props": { "data": { "key": "order.sneg", "value": "string(order.neg)" } } },
        { "id": "s2", "type": "expression", "props": { "data": { "key": "order.slist", "value": "map(order.list, string(#))" } } },
        { "id": "s3", "type": "expression", "props": { "data": { "key": "order.smatched", "value": "string(order.matched)" } } },
        { "id": "s4", "type": "expression", "props": { "data": { "key": "order.stneg", "value": "string(order.tneg)" } } },
        { "id": "s5", "type": "expression", "props": { "data": { "key": "order.stags", "value": "map(order.tags, string(#))" } } }
    ] });
    let inputs = [
        json!({ "order": { "amount": 0, "items": [0, 2] } }),
        json!({ "order": { "amount": 0, "items": [0] } }),
        json!({ "order": { "amount": 7, "items": [1] } }),
    ];
    let mismatches = Policies::mismatches("negative zero", doc, &inputs);
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn policy_columnar_entities_keep_field_order() {
    let inputs = [
        json!({ "customer": { "companies": [ { "revenue": 5 }, { "revenue": 50 } ] } }),
        json!({ "customer": { "companies": [ { "revenue": 20 } ] } }),
    ];
    let mismatches: Vec<String> = [false, true]
        .into_iter()
        .flat_map(|nested| {
            let mut blocks = vec![
                json!({ "id": "dm-customer", "type": "dataModel", "props": { "data": {
                    "name": "customer",
                    "properties": [ { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false } ]
                } } }),
                json!({ "id": "dm-company", "type": "dataModel", "props": { "data": {
                    "name": "company",
                    "properties": [ { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false } ]
                } } }),
                json!({ "id": "e1", "type": "expression", "props": { "data": { "key": "company.meta.big", "value": "company.revenue > 10" } } }),
                json!({ "id": "e2", "type": "expression", "props": { "data": { "key": "company.flag", "value": "company.meta.big ? 'y' : 'n'" } } }),
                json!({ "id": "k1", "type": "expression", "props": { "data": { "key": "customer.keys", "value": "map(customer.companies, keys(#))" } } }),
            ];
            if nested {
                blocks.push(json!({ "id": "e3", "type": "expression", "props": { "data": { "key": "customer.info.count", "value": "len(customer.companies)" } } }));
                blocks.push(json!({ "id": "e4", "type": "expression", "props": { "data": { "key": "customer.info.first", "value": "customer.info.count > 0" } } }));
            }
            Policies::mismatches(&format!("key order nested={nested}"), json!({ "blocks": blocks }), &inputs)
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn policy_collects_in_output_order() {
    let ws = Policies::workspace(json!({ "blocks": [
        { "id": "dm", "type": "dataModel", "props": { "data": {
            "name": "order",
            "properties": [ { "id": "p1", "name": "amount", "type": "number", "array": false, "optional": false } ]
        } } },
        { "id": "dt", "type": "decisionTable", "props": { "data": {
            "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
            "outputs": [
                { "id": "o1", "name": "", "field": "order.a[]" },
                { "id": "o2", "name": "", "field": "order.b[]" },
                { "id": "o3", "name": "", "field": "order.c[]" },
                { "id": "o4", "name": "", "field": "order.d[]" },
                { "id": "o5", "name": "", "field": "order.e[]" }
            ],
            "rules": [ { "i1": "", "o1": "1", "o2": "2", "o3": "3", "o4": "4", "o5": "5" } ]
        } } }
    ] }));
    let request = Policies::request(&json!({ "order": { "amount": 1 } }), true);
    let writes = |result: EvaluationResult| -> Vec<String> {
        result
            .trace
            .map(|trace| {
                trace
                    .as_legacy()
                    .expect("legacy trace")
                    .executions
                    .iter()
                    .flat_map(|execution| {
                        execution.writes.iter().map(|write| write.path.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let output = Policies::text(ws.evaluate(&request));
    let log = writes(ws.enhance_trace(&request).unwrap());
    assert_eq!(log, ["order.a", "order.b", "order.c", "order.d", "order.e"]);
    let unordered = |text: &str| serde_json::from_str::<Value>(text).ok();
    let mut driver_log = writes(ws.enhance_trace_with_driver(&request).unwrap());
    driver_log.sort();
    assert_eq!(driver_log, log);
    assert_eq!(
        unordered(&Policies::text(ws.evaluate_with_driver(&request))),
        unordered(&output)
    );
    for run in 0..32 {
        assert_eq!(
            Policies::text(ws.evaluate(&request)),
            output,
            "run {run}: output order"
        );
        assert_eq!(
            writes(ws.enhance_trace(&request).unwrap()),
            log,
            "run {run}: write log order"
        );
    }
}

#[tokio::test]
async fn sub_decisions_reuse_stripped_functions() {
    let loader = Arc::new(zen_engine::loader::MemoryLoader::default());
    loader.add("child", Stripped::primed());
    let graph = Graphs::chain(vec![
        json!({ "id": "d", "name": "d", "type": "decisionNode", "content": { "key": "child" } }),
    ]);
    let mut parent = Graphs::decision(&graph).with_loader(loader);
    parent.compile();
    let input = Variable::from(json!({ "x": 1 }));
    let top = Graphs::text(
        Decision::from(Arc::new(Stripped::primed()))
            .evaluate(input.clone())
            .await,
    );
    assert!(
        top.contains("cache"),
        "top-level function must use the stripped cache: {top}"
    );
    let single = Graphs::text(parent.evaluate(input.clone()).await);
    let batch: Vec<String> = parent
        .evaluate_batch(&[input.clone(), input], EvaluationOptions::default())
        .await
        .into_iter()
        .map(Graphs::text)
        .collect();
    assert!(
        single.contains("cache") && batch.iter().all(|row| row.contains("cache")),
        "sub-decision re-stripped the source: single {single} batch {batch:?}"
    );
}

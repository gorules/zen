use std::sync::Arc;

use serde_json::{json, Value};
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::policy::{BlockExecution, BlockTrace, GraphTraceMap, Trace, Workspace};
use zen_engine::{DecisionEngine, EvaluationOptions};

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn edge(id: &str, source: &str, target: &str) -> Value {
    json!({ "id": id, "sourceId": source, "targetId": target, "sourceHandle": null })
}

fn expression_node(id: &str, rows: &[(&str, &str)], attributes: Value) -> Value {
    let expressions: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({ "id": format!("{id}-e{i}"), "key": key, "value": value }))
        .collect();
    let mut content = json!({ "expressions": expressions });
    merge(&mut content, attributes);
    node(id, "expressionNode", content)
}

fn merge(target: &mut Value, extra: Value) {
    let (Some(target), Some(extra)) = (target.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (key, value) in extra {
        target.insert(key.clone(), value.clone());
    }
}

fn linear_graph(middle: Vec<Value>) -> Value {
    let mut nodes = vec![node("in", "inputNode", json!({}))];
    let mut edges = Vec::new();
    let mut prev = "in".to_string();
    for n in middle {
        let id = n["id"].as_str().unwrap().to_string();
        edges.push(edge(&format!("edge-{prev}-{id}"), &prev, &id));
        nodes.push(n);
        prev = id;
    }
    nodes.push(node("out", "outputNode", json!({})));
    edges.push(edge(&format!("edge-{prev}-out"), &prev, "out"));
    json!({ "nodes": nodes, "edges": edges })
}

async fn graph_trace(documents: &[(&str, Value)], entry: &str, input: Value) -> GraphTraceMap {
    let loader = Arc::new(MemoryLoader::default());
    for (key, value) in documents {
        loader.add(*key, document(value.clone()));
    }
    let engine = DecisionEngine::default().with_loader(loader);
    let response = engine
        .evaluate_with_opts(
            entry,
            input.into(),
            EvaluationOptions {
                trace: true,
                max_depth: 10,
            },
        )
        .await
        .expect("evaluation succeeds");
    response
        .trace
        .expect("trace present")
        .into_graph()
        .expect("graph trace")
}

async fn enhance(documents: &[(&str, Value)], entry: &str, input: Value) -> Trace {
    let trace = graph_trace(documents, entry, input.clone()).await;
    let mut ws = Workspace::new();
    for (key, value) in documents {
        ws.set_document(*key, document(value.clone()));
    }
    ws.enhance_graph_trace(&Arc::from(entry), &trace)
        .expect("enhance succeeds")
}

fn execution<'a>(trace: &'a Trace, block_id: &str) -> &'a BlockExecution {
    trace
        .executions
        .iter()
        .find(|ex| ex.block_id.as_ref() == block_id)
        .unwrap_or_else(|| panic!("execution {block_id} missing"))
}

fn sorted_reads(execution: &BlockExecution) -> Vec<String> {
    let mut reads: Vec<String> = execution.reads.iter().map(|r| r.to_string()).collect();
    reads.sort();
    reads
}

#[tokio::test]
async fn expression_rows_get_ast_reads_including_dollar_refs() {
    let docs = [(
        "g",
        linear_graph(vec![expression_node(
            "calc",
            &[
                ("priorFee", "ledger.prior"),
                (
                    "feeEntry",
                    "inject ? { fee: merge([$.priorFee, { roundedTotal: round(($.priorFee.sum ?? 0) + surcharge, 2) }]) } : {}",
                ),
            ],
            json!({}),
        )]),
    )];
    let trace = enhance(
        &docs,
        "g",
        json!({ "ledger": { "prior": { "sum": 1 } }, "surcharge": 2, "inject": true }),
    )
    .await;

    let first = execution(&trace, "calc:calc-e0");
    assert!(
        matches!(&first.trace, BlockTrace::Expression { property, .. } if property.as_ref() == "priorFee")
    );
    assert_eq!(sorted_reads(first), vec!["ledger.prior"]);

    let second = execution(&trace, "calc:calc-e1");
    assert_eq!(
        sorted_reads(second),
        vec!["inject", "priorFee", "priorFee.sum", "surcharge"]
    );
    assert_eq!(
        second.operand_values.get("surcharge"),
        Some(&json!(2).into())
    );
    assert_eq!(
        second.operand_values.get("$.priorFee.sum"),
        Some(&json!(1).into())
    );
    assert!(trace.properties.contains_key("feeEntry"));
}

#[tokio::test]
async fn decision_table_maps_rows_reads_and_evaluations() {
    let table = node(
        "pricing",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputs": [{ "id": "c1", "name": "Total", "field": "cart.total" }],
            "outputs": [{ "id": "o1", "name": "Discount", "field": "discount" }],
            "rules": [
                { "c1": "> 500", "o1": "0.2" },
                { "c1": "", "o1": "0.1" }
            ]
        }),
    );
    let docs = [("g", linear_graph(vec![table]))];
    let trace = enhance(&docs, "g", json!({ "cart": { "total": 100 } })).await;

    let table = execution(&trace, "pricing");
    let BlockTrace::DecisionTable {
        matched_rows,
        evaluations,
        ..
    } = &table.trace
    else {
        panic!("expected decision table trace");
    };
    assert_eq!(matched_rows, &vec![1]);
    assert_eq!(evaluations.len(), 1);
    assert_eq!(evaluations[0].get("discount"), Some(&json!(0.1).into()));
    assert!(sorted_reads(table).contains(&"cart.total".to_string()));
    assert_eq!(trace.properties.get("discount"), Some(&json!(0.1).into()));
}

#[tokio::test]
async fn switch_records_arms_and_condition_reads() {
    let switch = node(
        "route",
        "switchNode",
        json!({ "statements": [
            { "id": "s1", "condition": "fee > 5" },
            { "id": "s2", "condition": "" }
        ]}),
    );
    let branch_a = expression_node("high", &[("tier", "'high'")], json!({}));
    let branch_b = expression_node("low", &[("tier", "'low'")], json!({}));
    let graph = json!({
        "nodes": [
            node("in", "inputNode", json!({})),
            switch,
            branch_a,
            branch_b,
            node("out", "outputNode", json!({}))
        ],
        "edges": [
            edge("e1", "in", "route"),
            { "id": "e2", "sourceId": "route", "targetId": "high", "sourceHandle": "s1" },
            { "id": "e3", "sourceId": "route", "targetId": "low", "sourceHandle": "s2" },
            edge("e4", "high", "out"),
            edge("e5", "low", "out")
        ]
    });
    let docs = [("g", graph)];
    let trace = enhance(&docs, "g", json!({ "fee": 10 })).await;

    let switch = execution(&trace, "route");
    let BlockTrace::Match {
        matched_arm, arms, ..
    } = &switch.trace
    else {
        panic!("expected match trace");
    };
    assert_eq!(matched_arm.as_deref(), Some("s1"));
    assert_eq!(
        arms.iter().map(|arm| arm.result).collect::<Vec<_>>(),
        vec![true, false]
    );
    assert_eq!(sorted_reads(switch), vec!["fee"]);
    assert_eq!(switch.operand_values.get("fee"), Some(&json!(10).into()));
}

#[tokio::test]
async fn sub_decision_recurses_with_prefixed_ids_and_free_reads() {
    let sub = linear_graph(vec![expression_node(
        "risk",
        &[("risk", "total > 500 ? 'high' : 'low'")],
        json!({}),
    )]);
    let main = linear_graph(vec![node("call", "decisionNode", json!({ "key": "sub" }))]);
    let docs = [("g", main), ("sub", sub)];
    let trace = enhance(&docs, "g", json!({ "total": 600 })).await;

    let group = execution(&trace, "call");
    assert_eq!(sorted_reads(group), vec!["total"]);
    assert!(group
        .writes
        .iter()
        .any(|write| write.path.as_ref() == "risk"));

    let nested = execution(&trace, "call/risk:risk-e0");
    assert!(
        matches!(&nested.trace, BlockTrace::Expression { property, .. } if property.as_ref() == "risk")
    );
    assert_eq!(sorted_reads(nested), vec!["total"]);
    assert_eq!(trace.properties.get("risk"), Some(&json!("high").into()));
}

#[tokio::test]
async fn looped_sub_decision_gets_instance_paths_and_mapped_reads() {
    let sub = linear_graph(vec![expression_node(
        "risk",
        &[("risk", "total > 500 ? 'high' : 'low'")],
        json!({}),
    )]);
    let main = linear_graph(vec![node(
        "call",
        "decisionNode",
        json!({
            "key": "sub",
            "inputField": "items",
            "outputPath": "results",
            "executionMode": "loop"
        }),
    )]);
    let docs = [("g", main), ("sub", sub)];
    let trace = enhance(
        &docs,
        "g",
        json!({ "items": [{ "total": 600 }, { "total": 100 }] }),
    )
    .await;

    let group = execution(&trace, "call");
    assert_eq!(sorted_reads(group), vec!["items.total"]);
    assert!(group
        .writes
        .iter()
        .any(|write| write.path.as_ref() == "results"));

    let first = execution(&trace, "call[0]/risk:risk-e0");
    assert_eq!(first.instance_path.as_deref(), Some("items.0"));
    assert!(
        matches!(&first.trace, BlockTrace::Expression { value, .. } if *value == json!("high").into())
    );
    let second = execution(&trace, "call[1]/risk:risk-e0");
    assert_eq!(second.instance_path.as_deref(), Some("items.1"));
    assert!(
        matches!(&second.trace, BlockTrace::Expression { value, .. } if *value == json!("low").into())
    );
}

#[tokio::test]
async fn looped_table_extracts_per_iteration_evaluations_under_output_path() {
    let table = node(
        "pricing",
        "decisionTableNode",
        json!({
            "hitPolicy": "first",
            "inputField": "carts",
            "outputPath": "discounts",
            "executionMode": "loop",
            "inputs": [{ "id": "c1", "name": "Total", "field": "total" }],
            "outputs": [{ "id": "o1", "name": "Discount", "field": "discount" }],
            "rules": [
                { "c1": "> 500", "o1": "0.2" },
                { "c1": "", "o1": "0.1" }
            ]
        }),
    );
    let docs = [("g", linear_graph(vec![table]))];
    let trace = enhance(
        &docs,
        "g",
        json!({ "carts": [{ "total": 600 }, { "total": 100 }] }),
    )
    .await;

    let iterations: Vec<&BlockExecution> = trace
        .executions
        .iter()
        .filter(|ex| ex.block_id.as_ref() == "pricing")
        .collect();
    assert_eq!(iterations.len(), 2);
    assert_eq!(iterations[0].instance_path.as_deref(), Some("carts.0"));
    assert_eq!(iterations[1].instance_path.as_deref(), Some("carts.1"));

    let BlockTrace::DecisionTable {
        matched_rows,
        evaluations,
        ..
    } = &iterations[0].trace
    else {
        panic!("expected decision table trace");
    };
    assert_eq!(matched_rows, &vec![0]);
    assert_eq!(
        evaluations[0].get("discounts.discount"),
        Some(&json!(0.2).into())
    );
    assert!(sorted_reads(iterations[0]).contains(&"carts.total".to_string()));
    assert_eq!(
        iterations[0].operand_values.get("total"),
        Some(&json!(600).into())
    );
    assert_eq!(
        iterations[1].operand_values.get("total"),
        Some(&json!(100).into())
    );
}

use serde_json::json;
use std::sync::Arc;
use std::time::Instant;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::{ColumnStore, DecisionEngine, EvaluationOptions, Variable};
use zen_expression::lane::{Column, Columns, Values};
use zen_expression::{BatchExpression, Isolate};

const ROWS: usize = 100_000;

fn graph() -> DecisionContent {
    serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "type": "inputNode", "name": "Request", "position": { "x": 0, "y": 0 } },
            { "id": "ex", "type": "expressionNode", "name": "Features", "position": { "x": 0, "y": 0 },
              "content": { "expressions": [
                { "id": "e1", "key": "amount_major", "value": "txn.amount / 100" },
                { "id": "e2", "key": "risky", "value": "txn.amount > 50000 and txn.channel == 'ecom'" },
                { "id": "e3", "key": "spend_ratio", "value": "txn.amount / (txn.avg_ticket ?? 1)" },
                { "id": "e4", "key": "band", "value": "txn.amount > 100000 ? 'high' : txn.amount > 10000 ? 'mid' : 'low'" }
              ] } },
            { "id": "out", "type": "outputNode", "name": "Response", "position": { "x": 0, "y": 0 } }
        ],
        "edges": [
            { "id": "a", "sourceId": "in", "targetId": "ex", "type": "edge" },
            { "id": "b", "sourceId": "ex", "targetId": "out", "type": "edge" }
        ]
    }))
    .expect("graph")
}

fn inputs() -> Vec<Variable> {
    let channels = ["ecom", "pos", "atm"];
    (0..ROWS)
        .map(|i| {
            Variable::from(json!({
                "txn": {
                    "amount": (i * 7919) % 200_000,
                    "channel": channels[i % 3],
                    "avg_ticket": if i % 10 == 0 { serde_json::Value::Null } else { json!(1000 + i % 5000) },
                    "items": [{ "v": i % 3 }, { "v": i % 5 }, { "v": null }, { "v": 7 }]
                }
            }))
        })
        .collect()
}

fn rate(label: &str, rows: usize, started: Instant) {
    let seconds = started.elapsed().as_secs_f64();
    println!("{label:<44} {rows:>7} rows {seconds:>8.3}s {:>12.0} rows/s", rows as f64 / seconds);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("features.json", graph());
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(failures.is_empty(), "{failures:?}");
    let inputs = inputs();

    let started = Instant::now();
    let batch = engine.evaluate_many("features.json", &inputs).await;
    rate("decision evaluate_many (compiled, batch)", inputs.len(), started);
    assert!(batch.iter().all(Result::is_ok));

    let sample = &inputs[..10_000];
    let started = Instant::now();
    let mut rows = Vec::with_capacity(sample.len());
    for input in sample {
        rows.push(engine.evaluate("features.json", input.clone()).await.expect("row").result);
    }
    rate("decision evaluate per row", sample.len(), started);
    for (row, got) in rows.iter().zip(&batch) {
        assert_eq!(row.to_value(), got.as_ref().expect("batch").to_value());
    }

    for source in [
        "txn.amount / (txn.avg_ticket ?? 1) > 3 and txn.channel != 'atm'",
        "txn.amount > 100000 ? 'high' : txn.amount > 10000 ? 'mid' : 'low'",
        "sum(txn.items as t, t.v, t.v > 1) + count_distinct(txn.items as t, t.v)",
    ] {
        let expression = BatchExpression::standard(source).expect("compile");
        let started = Instant::now();
        let values = expression.evaluate(&inputs);
        rate(&format!("BatchExpression lane={}", expression.is_lane()), inputs.len(), started);

        let started = Instant::now();
        let mut isolate = Isolate::new();
        let compiled = isolate.compile_standard(source).expect("compile");
        let mut per_row = Vec::with_capacity(inputs.len());
        for input in &inputs {
            per_row.push(compiled.evaluate(input.clone()));
        }
        rate("Expression::evaluate per row (stack VM)", inputs.len(), started);
        for (row, got) in per_row.iter().zip(&values) {
            assert_eq!(row.as_ref().ok(), got.as_ref().ok(), "{source}");
        }
        println!("  {source}");
    }

    let amounts: Vec<i64> = (0..ROWS as i64).map(|i| (i * 7919) % 200_000).collect();
    let tickets: Vec<f64> = (0..ROWS).map(|i| (1000 + i % 5000) as f64).collect();
    let channels: Vec<&str> = (0..ROWS).map(|i| ["ecom", "pos", "atm"][i % 3]).collect();
    let columns = Columns::new(ROWS)
        .column("txn.amount", Column::new(Values::I64(&amounts)))
        .column("txn.avg_ticket", Column::new(Values::F64(&tickets)))
        .column("txn.channel", Column::new(Values::Strs(&channels)));
    for source in [
        "txn.amount / (txn.avg_ticket ?? 1) > 3 and txn.channel != 'atm'",
        "txn.amount > 100000 ? 'high' : txn.amount > 10000 ? 'mid' : 'low'",
    ] {
        let expression = BatchExpression::standard(source).expect("compile");
        let started = Instant::now();
        let values = expression.evaluate_columns(&columns);
        rate("BatchExpression::evaluate_columns", ROWS, started);
        assert!(values.iter().all(Result::is_ok));
        println!("  {source}");
    }

    let store = ColumnStore::new();
    let started = Instant::now();
    let output = engine
        .evaluate_columns("features.json", &columns, &store, EvaluationOptions::default())
        .await;
    rate("decision evaluate_columns (columnar)", ROWS, started);
    println!(
        "  typed {} laned {} hosted {} errors {}",
        output.typed,
        output.laned,
        output.hosted,
        output.errors.iter().filter(|e| e.is_some()).count()
    );
    let started = Instant::now();
    let rows: Vec<Variable> = (0..ROWS).map(|row| output.row(row)).collect();
    rate("  materializing rows from ColumnarOutput", rows.len(), started);
}

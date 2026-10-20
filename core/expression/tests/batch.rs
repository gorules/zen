use serde_json::json;
use zen_expression::{evaluate_expression_batch, BatchExpression, Isolate, Variable};

fn inputs(rows: usize) -> Vec<Variable> {
    (0..rows)
        .map(|i| {
            json!({
                "amount": (i * 37) % 1000,
                "channel": if i % 2 == 0 { "ecom" } else { "pos" },
                "items": [{ "v": i % 5 }, { "v": null }, { "v": 3 }],
                "maybe": if i % 7 == 0 { serde_json::Value::Null } else { json!(i) }
            })
            .into()
        })
        .collect()
}

fn per_row(source: &str, inputs: &[Variable]) -> Vec<Result<serde_json::Value, String>> {
    inputs
        .iter()
        .map(|input| {
            Isolate::with_environment(input.clone())
                .run_standard(source)
                .map(|v| v.to_value())
                .map_err(|e| e.to_string())
        })
        .collect()
}

#[test]
fn batch_matches_row_evaluation() {
    let sources = [
        "amount / 100",
        "amount > 500 and channel == 'ecom'",
        "maybe ?? -1",
        "sum(items as t, t.v)",
        "stddev(items as t, t.v)",
        "mode(items, #.v)",
        "first(items as t, t.v) + last(items as t, t.v)",
        "count_distinct(items as t, t.v)",
        "percentile(items as t, t.v, 0.5)",
        "topK(items as t, t.v, 1)",
        "maybe / (amount - amount)",
        "{ big: amount > 900, label: channel + '-' + string(amount) }",
    ];
    for rows in [1, 63, 64, 65, 200] {
        let inputs = inputs(rows);
        for source in sources {
            let expected = per_row(source, &inputs);
            let got: Vec<Result<serde_json::Value, String>> = evaluate_expression_batch(source, &inputs)
                .unwrap()
                .into_iter()
                .map(|r| r.map(|v| v.to_value()).map_err(|e| e.to_string()))
                .collect();
            assert_eq!(got, expected, "{source} over {rows} rows");
        }
    }
}

#[test]
fn unary_batches_return_booleans() {
    let expression = BatchExpression::unary("> 10").unwrap();
    let inputs: Vec<Variable> = [5, 10, 11, 100].iter().map(|n| json!({ "$": n }).into()).collect();
    let got: Vec<serde_json::Value> = expression
        .evaluate(&inputs)
        .into_iter()
        .map(|r| r.unwrap().to_value())
        .collect();
    assert_eq!(got, vec![json!(false), json!(false), json!(true), json!(true)]);
}

#[test]
fn syntax_errors_fail_the_compile() {
    assert!(BatchExpression::standard("amount +").is_err());
    assert!(evaluate_expression_batch("(", &inputs(3)).is_err());
}

#[test]
fn columnar_batch_matches_row_evaluation() {
    use zen_expression::lane::{Column, Columns, Values};
    let rows = 150;
    let amounts: Vec<i64> = (0..rows as i64).map(|i| (i * 37) % 1000).collect();
    let tickets: Vec<f64> = (0..rows).map(|i| 10.0 + (i % 9) as f64).collect();
    let validity: Vec<u64> = vec![!0b1001u64, !0u64, !0u64];
    let channels: Vec<&str> = (0..rows).map(|i| if i % 3 == 0 { "atm" } else { "ecom" }).collect();
    let columns = Columns::new(rows)
        .column("txn.amount", Column::new(Values::I64(&amounts)))
        .column("txn.avg_ticket", Column::with_validity(Values::F64(&tickets), &validity, 0))
        .column("txn.channel", Column::new(Values::Strs(&channels)));
    for source in [
        "txn.amount / (txn.avg_ticket ?? 1) > 3 and txn.channel != 'atm'",
        "txn.amount > 500 ? 'high' : 'low'",
        "txn.avg_ticket",
        "txn.channel + '-' + string(txn.amount)",
    ] {
        let expression = BatchExpression::standard(source).unwrap();
        let got: Vec<_> = expression
            .evaluate_columns(&columns)
            .into_iter()
            .map(|r| r.map(|v| v.to_value()).map_err(|e| e.to_string()))
            .collect();
        let inputs: Vec<Variable> = (0..rows).map(|row| columns.row(row)).collect();
        assert_eq!(got, per_row(source, &inputs), "{source}");
    }
}

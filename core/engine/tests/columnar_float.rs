use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::columns::{Column, Columns, Dictionary, Float64, Values};
use zen_engine::model::GraphContent;
use zen_engine::{Decision, EngineVersion, EvaluationOptions, Variable, Workspace};

const ROWS: usize = 400;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

struct Data {
    price: Vec<f64>,
    qty: Vec<f64>,
    rate: Vec<f64>,
    odd: Vec<f64>,
    masked: Vec<f64>,
    valid: Vec<u64>,
    offsets: Vec<i32>,
    amount: Vec<f64>,
}

impl Data {
    fn new() -> Data {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let edges = [
            0.0,
            -0.0,
            0.1 + 0.2,
            1.0 / 3.0,
            1e15,
            1e20,
            123456789012345.67,
            9_007_199_254_740_993.0,
            1.2345678901234567e-12,
            1e-7,
            5e-324,
            -99.99,
            12.5,
            100.0,
            300.0,
            299.99999999999994,
        ];
        let mut data = Data {
            price: Vec::new(),
            qty: Vec::new(),
            rate: Vec::new(),
            odd: Vec::new(),
            masked: Vec::new(),
            valid: vec![0u64; ROWS.div_ceil(64)],
            offsets: vec![0],
            amount: Vec::new(),
        };
        for row in 0..ROWS {
            data.price.push(match row < edges.len() {
                true => edges[row],
                false => rng.below(50_000) as f64 / 100.0,
            });
            data.qty.push(rng.below(20) as f64);
            data.rate.push(match rng.below(4) {
                0 => rng.below(10_000) as f64 / 10_000.0,
                1 => rng.below(1_000_000) as f64 / 7.0,
                2 => -(rng.below(1_000) as f64) / 8.0,
                _ => rng.below(100) as f64 * 1e-9,
            });
            data.odd.push(match rng.below(6) {
                0 => f64::NAN,
                1 => f64::INFINITY,
                2 => f64::NEG_INFINITY,
                3 => 1e300,
                _ => rng.below(1_000) as f64 / 4.0,
            });
            data.masked.push(match rng.below(5) {
                0 => f64::NAN,
                _ => rng.below(100_000) as f64 / 1_000.0,
            });
            if data.masked[row].is_finite() && rng.below(4) != 0 {
                data.valid[row / 64] |= 1 << (row % 64);
            }
            for _ in 0..rng.below(4) {
                data.amount.push(match rng.below(8) {
                    0 => 1.0 / (1.0 + rng.below(9) as f64),
                    _ => rng.below(100_000) as f64 / 100.0,
                });
            }
            data.offsets.push(data.amount.len() as i32);
        }
        data
    }
}

fn graph() -> Decision {
    let expressions = [
        ("total", "price * qty"),
        ("cheap", "price < 100"),
        ("rounded", "round(price, 1)"),
        ("text", "string(price)"),
        ("same", "price == 12.5"),
        ("scaled", "rate * 100"),
        ("rate", "rate"),
        ("odd", "odd"),
        ("oddly", "odd == null ? -1 : odd + 1"),
        ("masked", "masked == null ? 'none' : string(masked)"),
        ("amounts", "map(items, #.amount * 2)"),
        ("sum", "sum(map(items, #.amount))"),
        ("items", "items"),
    ];
    let list: Vec<Value> = expressions
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({"id": format!("x{i}"), "key": key, "value": value}))
        .collect();
    let content: GraphContent = serde_json::from_value(json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            {"id": "expr", "name": "expr", "type": "expressionNode", "content": {"expressions": list, "passThrough": true}},
            {"id": "table", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "passThrough": true,
                "inputs": [{"id": "i0", "field": "price"}, {"id": "i1", "field": "rate"}],
                "outputs": [{"id": "o0", "field": "tier"}],
                "rules": [
                    {"_id": "r0", "i0": "> 300", "i1": "> 0.5", "o0": "'gold'"},
                    {"_id": "r1", "i0": "[100..300]", "i1": "", "o0": "'silver'"},
                    {"_id": "r2", "i0": "12.5", "i1": "", "o0": "'exact'"},
                    {"_id": "r3", "i0": "", "i1": "< 0", "o0": "'negative'"},
                    {"_id": "r4", "i0": "", "i1": "", "o0": "'basic'"}
                ]
            }},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "expr", "type": "edge"},
            {"id": "e2", "sourceId": "expr", "targetId": "table", "type": "edge"},
            {"id": "e3", "sourceId": "table", "targetId": "out", "type": "edge"}
        ]
    }))
    .unwrap();
    Decision::from(Arc::new(content))
}

fn options() -> EvaluationOptions {
    EvaluationOptions {
        now: zen_engine::context::parse_instant("2025-06-15T12:00:00Z"),
        ..Default::default()
    }
}

fn canon(value: &Variable) -> Value {
    match value {
        Variable::Null => Value::Null,
        Variable::Bool(b) => json!(b),
        Variable::Number(n) => json!(format!("#{n}")),
        Variable::String(s) => json!(s.as_str()),
        Variable::Array(items) => Value::Array(items.borrow().iter().map(canon).collect()),
        Variable::Object(map) => Value::Object(
            map.borrow()
                .iter()
                .map(|(k, v)| (k.to_string(), canon(v)))
                .collect(),
        ),
        Variable::Dynamic(d) => json!(d.to_string()),
    }
}

struct Floats<'a> {
    price: Float64<'a>,
    qty: Float64<'a>,
    rate: Float64<'a>,
    odd: Float64<'a>,
    masked: Float64<'a>,
    amount: Float64<'a>,
}

impl<'a> Floats<'a> {
    fn new(data: &'a Data) -> Floats<'a> {
        Floats {
            price: Float64::new(&data.price),
            qty: Float64::new(&data.qty),
            rate: Float64::new(&data.rate),
            odd: Float64::new(&data.odd),
            masked: Float64::new(&data.masked),
            amount: Float64::new(&data.amount),
        }
    }
}

fn columns<'a>(
    data: &'a Data,
    values: [Values<'a>; 5],
    child: &'a Column<'a>,
    prefix: [&'a str; 6],
    builder: bool,
) -> Columns<'a> {
    let [price, qty, rate, odd, masked] = values;
    let list = vec![
        (prefix[0], Column::new(price)),
        (prefix[1], Column::new(qty)),
        (prefix[2], Column::new(rate)),
        (prefix[3], Column::new(odd)),
        (prefix[4], Column::with_validity(masked, &data.valid, 0)),
        (
            prefix[5],
            Column::new(Values::List {
                offsets: &data.offsets,
                child: Dictionary::Column(child),
            }),
        ),
    ];
    match builder {
        true => list
            .into_iter()
            .fold(Columns::new(ROWS), |columns, (path, column)| {
                columns.column(path, column)
            }),
        false => Columns {
            rows: ROWS,
            columns: list,
        },
    }
}

const PLAIN: [&str; 6] = ["price", "qty", "rate", "odd", "masked", "items"];
const NESTED: [&str; 6] = [
    "order.price",
    "order.qty",
    "order.rate",
    "order.odd",
    "order.masked",
    "order.items",
];

fn with_fixture(prefix: [&'static str; 6], run: impl FnOnce(&Columns, &Columns, &Columns)) {
    let data = Data::new();
    let floats = Floats::new(&data);
    let again = Floats::new(&data);
    let plain_items = [("amount", Column::new(Values::F64(&data.amount)))];
    let float_items = [("amount", Column::new(Values::Float64(&floats.amount)))];
    let plain_child = Column::new(Values::Struct {
        fields: &plain_items,
        len: data.amount.len(),
    });
    let float_child = Column::new(Values::Struct {
        fields: &float_items,
        len: data.amount.len(),
    });
    let expected = columns(
        &data,
        [
            Values::F64(&data.price),
            Values::F64(&data.qty),
            Values::F64(&data.rate),
            Values::F64(&data.odd),
            Values::F64(&data.masked),
        ],
        &plain_child,
        prefix,
        true,
    );
    let built = columns(
        &data,
        [
            Values::Float64(&floats.price),
            Values::Float64(&floats.qty),
            Values::Float64(&floats.rate),
            Values::Float64(&floats.odd),
            Values::Float64(&floats.masked),
        ],
        &float_child,
        prefix,
        true,
    );
    let literal = columns(
        &data,
        [
            Values::Float64(&again.price),
            Values::Float64(&again.qty),
            Values::Float64(&again.rate),
            Values::Float64(&again.odd),
            Values::Float64(&again.masked),
        ],
        &float_child,
        prefix,
        false,
    );
    run(&expected, &built, &literal);
}

fn evaluate(decision: &Decision, columns: &Columns) -> Vec<Value> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let output = runtime.block_on(decision.evaluate_columns(columns, options()));
    (0..ROWS)
        .map(|row| match &output.errors[row] {
            Some(_) => json!({"error": true}),
            None => canon(&output.row(row)),
        })
        .collect()
}

fn json_row(data: &Data, row: usize) -> Value {
    let number = |f: f64| match (f.is_finite(), f == f.trunc() && f.abs() < 9e18) {
        (false, _) => Value::Null,
        (true, true) => json!(f as i64),
        (true, false) => json!(f),
    };
    let (a, b) = (data.offsets[row] as usize, data.offsets[row + 1] as usize);
    let mut object = json!({
        "price": number(data.price[row]),
        "qty": number(data.qty[row]),
        "rate": number(data.rate[row]),
        "odd": number(data.odd[row]),
        "items": data.amount[a..b].iter().map(|f| json!({"amount": number(*f)})).collect::<Vec<_>>(),
    });
    if data.valid[row / 64] >> (row % 64) & 1 == 1 {
        object["masked"] = number(data.masked[row]);
    }
    object
}

#[cfg(feature = "arbitrary_precision")]
fn parsed(text: &str) -> Option<Variable> {
    serde_json::from_str(text).ok()
}

#[cfg(not(feature = "arbitrary_precision"))]
fn parsed(_: &str) -> Option<Variable> {
    None
}

#[test]
fn float64_columns_materialise_like_f64_columns() {
    with_fixture(PLAIN, |expected, built, literal| {
        for row in 0..ROWS {
            let want = canon(&expected.row(row));
            assert_eq!(canon(&built.row(row)), want, "built row {row}");
            assert_eq!(canon(&literal.row(row)), want, "literal row {row}");
        }
    });
}

#[test]
fn float64_columns_evaluate_like_f64_columns() {
    with_fixture(PLAIN, |expected, built, literal| {
        let decision = graph().with_version(EngineVersion::V2);
        let want = evaluate(&decision, expected);
        let got_built = evaluate(&decision, built);
        let got_literal = evaluate(&decision, literal);
        let fine = want.iter().filter(|v| v.get("error").is_none()).count();
        assert!(fine > ROWS / 2, "{fine}");
        for row in 0..ROWS {
            assert_eq!(got_built[row], want[row], "built row {row}");
            assert_eq!(got_literal[row], want[row], "literal row {row}");
        }
    });
}

#[test]
fn float64_columns_evaluate_like_version_one() {
    let data = Data::new();
    with_fixture(PLAIN, |expected, built, _| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let v1 = graph().with_version(EngineVersion::V1);
        let got = evaluate(&graph().with_version(EngineVersion::V2), built);
        for (row, got) in got.iter().enumerate() {
            let single = |input: Variable| {
                runtime
                    .block_on(v1.evaluate_with_opts(input, options()))
                    .map(|response| canon(&response.result))
                    .unwrap_or_else(|_| json!({"error": true}))
            };
            assert_eq!(got, &single(expected.row(row)), "row {row}");
            let text = serde_json::to_string(&json_row(&data, row)).unwrap();
            let tiny = data.price[row] != 0.0 && data.price[row].abs() < 1e-28;
            if let Some(parsed) = parsed(&text).filter(|_| !tiny) {
                assert_eq!(got, &single(parsed), "json row {row}");
            }
        }
    });
}

fn pick<'a>(columns: &Columns<'a>) -> Columns<'a> {
    Columns {
        rows: ROWS,
        columns: columns
            .columns
            .iter()
            .filter(|(path, _)| !matches!(*path, "order.odd" | "order.items"))
            .cloned()
            .collect(),
    }
}

fn policy() -> Workspace {
    let property = |id: &str, name: &str| json!({"id": id, "name": name, "type": "number", "array": false, "optional": true});
    let expression = |id: &str, key: &str, value: &str| json!({"id": id, "type": "expression", "props": {"data": {"key": key, "value": value}}});
    let document = json!({"blocks": [
        {"id": "model", "type": "dataModel", "props": {"data": {"name": "order", "properties": [
            property("p1", "price"),
            property("p2", "qty"),
            property("p3", "rate"),
            property("p4", "masked"),
        ]}}},
        expression("e1", "order.total", "order.price * order.qty"),
        expression("e2", "order.cheap", "order.price < 100"),
        expression("e3", "order.interest", "round(order.price * order.rate, 2)"),
        expression("e4", "order.label", "order.masked == null ? 'none' : string(order.masked)"),
    ]});
    let mut ws = Workspace::new().with_version(EngineVersion::V2);
    ws.set_policy("policy", serde_json::from_value(document).unwrap());
    ws
}

#[test]
fn float64_columns_evaluate_policies_like_f64_columns_and_single_rows() {
    with_fixture(NESTED, |expected, built, _| {
        let (expected, built) = (pick(expected), pick(built));
        let ws = policy();
        let path: Arc<str> = Arc::from("policy");
        let got = ws
            .evaluate_columns_pinned(&path, &[], &built, &options())
            .unwrap();
        let want = ws
            .evaluate_columns_pinned(&path, &[], &expected, &options())
            .unwrap();
        let mut filled = 0;
        for row in 0..ROWS {
            let got_row = canon(&got.row(row));
            assert_eq!(
                got.errors[row].is_some(),
                want.errors[row].is_some(),
                "row {row}"
            );
            assert_eq!(got_row, canon(&want.row(row)), "row {row}");
            let request = zen_engine::policy::EvaluateRequest {
                policy_path: path.clone(),
                input: expected.row(row),
                goals: Vec::new(),
                trace: zen_engine::trace::TraceFormat::Off,
            };
            match ws.evaluate_pinned(&request, &options()) {
                Ok(single) => {
                    assert_eq!(got_row, canon(&single.output), "row {row}");
                    filled += usize::from(
                        got_row
                            .get("order")
                            .is_some_and(|o| o.get("total").is_some()),
                    );
                }
                Err(_) => assert!(got.errors[row].is_some(), "row {row}"),
            }
        }
        assert!(filled > ROWS / 2, "{filled}");
    });
}

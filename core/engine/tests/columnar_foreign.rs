use chrono::{SecondsFormat, TimeZone};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Arc;
use zen_engine::columns::{
    Column, Columns, Decimal128, Dictionary, Float64, Temporal, TimeUnit, Values,
};
use zen_engine::model::GraphContent;
use zen_engine::{Decision, EngineVersion, EvaluationOptions, Variable};

const ROWS: usize = 300;

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
    price: Vec<i128>,
    qty: Vec<i128>,
    wide: Vec<i128>,
    fine: Vec<i128>,
    neg: Vec<i128>,
    at: Vec<i64>,
    berlin: Vec<i64>,
    naive: Vec<i64>,
    day: Vec<i32>,
    clock: Vec<i64>,
    valid: Vec<u64>,
    offsets: Vec<i32>,
    amount: Vec<i128>,
    when: Vec<i64>,
}

impl Data {
    fn new() -> Data {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let mut data = Data {
            price: Vec::new(),
            qty: Vec::new(),
            wide: Vec::new(),
            fine: Vec::new(),
            neg: Vec::new(),
            at: Vec::new(),
            berlin: Vec::new(),
            naive: Vec::new(),
            day: Vec::new(),
            clock: Vec::new(),
            valid: vec![0u64; ROWS.div_ceil(64)],
            offsets: vec![0],
            amount: Vec::new(),
            when: Vec::new(),
        };
        for row in 0..ROWS {
            data.price.push(rng.below(50_000) as i128 - 1_000);
            data.qty.push(rng.below(20) as i128);
            data.wide.push(match row % 5 {
                0 => i128::from(i64::MAX) * 1_000 + row as i128,
                1 => (1i128 << 100) + row as i128,
                _ => rng.below(1_000_000) as i128,
            });
            data.fine.push(rng.below(1 << 40) as i128 * 1_000_000_007);
            data.neg.push(rng.below(1_000) as i128 - 500);
            data.at
                .push(1_600_000_000_000 + rng.below(200_000_000_000) as i64 + [0, 1, 120][row % 3]);
            data.berlin.push(
                (1_600_000_000 + rng.below(200_000_000) as i64) * 1_000_000_000
                    + [0, 5_000, 123_456_789][row % 3],
            );
            data.naive
                .push(1_600_000_000 + rng.below(200_000_000) as i64);
            data.day.push(18_000 + rng.below(3_000) as i32);
            data.clock.push(rng.below(86_400_000_000) as i64);
            if row % 7 != 3 {
                data.valid[row / 64] |= 1 << (row % 64);
            }
            for _ in 0..rng.below(4) {
                data.amount.push(rng.below(100_000) as i128);
                data.when.push(rng.below(20_000) as i64);
            }
            data.offsets.push(data.amount.len() as i32);
        }
        data
    }
}

fn decimal_text(value: i128, scale: i8) -> String {
    if scale < 0 {
        return format!("{value}{}", "0".repeat(scale.unsigned_abs() as usize));
    }
    let digits = value.unsigned_abs().to_string();
    let scale = scale as usize;
    let digits = match digits.len() <= scale {
        true => format!("{}{digits}", "0".repeat(scale + 1 - digits.len())),
        false => digits,
    };
    let (whole, fraction) = digits.split_at(digits.len() - scale);
    let sign = if value < 0 { "-" } else { "" };
    match fraction.is_empty() {
        true => format!("{sign}{whole}"),
        false => format!("{sign}{whole}.{fraction}"),
    }
}

fn reference_decimal(value: i128, scale: i8) -> Option<Decimal> {
    Decimal::from_str(&decimal_text(value, scale)).ok()
}

fn utc(millis: i64) -> String {
    chrono::Utc
        .timestamp_millis_opt(millis)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn berlin(nanos: i64) -> String {
    chrono_tz::Europe::Berlin
        .timestamp_nanos(nanos)
        .to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn naive(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .unwrap()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%S%.f")
        .to_string()
}

fn day(days: i32) -> String {
    (chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap() + chrono::Duration::days(days.into()))
        .format("%Y-%m-%d")
        .to_string()
}

fn clock(micros: i64) -> String {
    chrono::NaiveTime::from_num_seconds_from_midnight_opt(
        (micros / 1_000_000) as u32,
        (micros % 1_000_000 * 1_000) as u32,
    )
    .unwrap()
    .format("%H:%M:%S%.f")
    .to_string()
}

struct Texts {
    offsets: Vec<i32>,
    data: String,
}

impl Texts {
    fn of(values: impl Iterator<Item = String>) -> Texts {
        let mut texts = Texts {
            offsets: vec![0],
            data: String::new(),
        };
        for value in values {
            texts.data.push_str(&value);
            texts.offsets.push(texts.data.len() as i32);
        }
        texts
    }

    fn values(&self) -> Values<'_> {
        Values::Text {
            offsets: &self.offsets,
            data: &self.data,
        }
    }
}

struct Reference {
    price: Vec<Decimal>,
    qty: Vec<Decimal>,
    wide: Vec<Decimal>,
    wide_valid: Vec<u64>,
    fine: Vec<Decimal>,
    neg: Vec<Decimal>,
    at: Texts,
    berlin: Texts,
    naive: Texts,
    day: Texts,
    clock: Texts,
    amount: Vec<Decimal>,
    when: Texts,
}

impl Reference {
    fn new(data: &Data) -> Reference {
        let all = |values: &[i128], scale: i8| -> Vec<Decimal> {
            values
                .iter()
                .map(|v| reference_decimal(*v, scale).unwrap_or_default())
                .collect()
        };
        let mut wide_valid = vec![0u64; ROWS.div_ceil(64)];
        for (row, v) in data.wide.iter().enumerate() {
            if reference_decimal(*v, 2).is_some() {
                wide_valid[row / 64] |= 1 << (row % 64);
            }
        }
        Reference {
            price: all(&data.price, 2),
            qty: all(&data.qty, 0),
            wide: all(&data.wide, 2),
            wide_valid,
            fine: all(&data.fine, 30),
            neg: all(&data.neg, -3),
            at: Texts::of(data.at.iter().map(|v| utc(*v))),
            berlin: Texts::of(data.berlin.iter().map(|v| berlin(*v))),
            naive: Texts::of(data.naive.iter().map(|v| naive(*v))),
            day: Texts::of(data.day.iter().map(|v| day(*v))),
            clock: Texts::of(data.clock.iter().map(|v| clock(*v))),
            amount: all(&data.amount, 1),
            when: Texts::of(data.when.iter().map(|v| day(*v as i32))),
        }
    }
}

struct Foreign<'a> {
    price: Decimal128<'a>,
    qty: Decimal128<'a>,
    wide: Decimal128<'a>,
    fine: Decimal128<'a>,
    neg: Decimal128<'a>,
    at: Temporal<'a>,
    berlin: Temporal<'a>,
    naive: Temporal<'a>,
    day: Temporal<'a>,
    clock: Temporal<'a>,
    amount: Decimal128<'a>,
    when: Temporal<'a>,
}

impl<'a> Foreign<'a> {
    fn new(data: &'a Data, when: &'a [i32]) -> Foreign<'a> {
        Foreign {
            price: Decimal128::new(&data.price, 2),
            qty: Decimal128::new(&data.qty, 0),
            wide: Decimal128::new(&data.wide, 2),
            fine: Decimal128::new(&data.fine, 30),
            neg: Decimal128::new(&data.neg, -3),
            at: Temporal::timestamp(&data.at, TimeUnit::Millisecond, Some("UTC")).unwrap(),
            berlin: Temporal::timestamp(&data.berlin, TimeUnit::Nanosecond, Some("Europe/Berlin"))
                .unwrap(),
            naive: Temporal::timestamp(&data.naive, TimeUnit::Second, None).unwrap(),
            day: Temporal::date32(&data.day),
            clock: Temporal::time64(&data.clock, TimeUnit::Microsecond),
            amount: Decimal128::new(&data.amount, 1),
            when: Temporal::date32(when),
        }
    }
}

const PATHS: [&str; 11] = [
    "price", "qty", "wide", "fine", "neg", "at", "berlin", "naive", "day", "clock", "items",
];

fn columns<'a>(
    values: [Values<'a>; 11],
    data: &'a Data,
    wide_valid: Option<&'a [u64]>,
    builder: bool,
) -> Columns<'a> {
    let mut list: Vec<(&'a str, Column<'a>)> = Vec::new();
    for (i, (path, values)) in PATHS.iter().zip(values).enumerate() {
        let column = match (i, wide_valid) {
            (2, Some(bits)) => Column::with_validity(values, bits, 0),
            (5 | 6, _) => Column::with_validity(values, &data.valid, 0),
            _ => Column::new(values),
        };
        list.push((path, column));
    }
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

fn graph() -> Decision {
    let expressions = [
        ("total", "price * qty"),
        ("cheap", "price < 100"),
        ("rounded", "round(price, 1)"),
        ("big", "wide == null ? 0 : wide + 1"),
        ("fine", "fine * 2"),
        ("neg", "neg + 1"),
        ("at", "at"),
        ("year", "at == null ? 0 : d(at).year()"),
        ("late", "at != null and d(at) > d('2024-01-01')"),
        ("length", "at == null ? 0 : len(at)"),
        ("local", "berlin"),
        ("hour", "berlin == null ? -1 : d(berlin).hour()"),
        ("naive", "naive"),
        ("naiveDay", "d(naive).day()"),
        ("day", "day"),
        ("dayText", "d(day).format('%Y/%m/%d')"),
        ("clock", "clock"),
        ("amounts", "map(items, #.amount * 2)"),
        ("whens", "map(items, #.when)"),
        ("sum", "sum(map(items, #.amount))"),
    ];
    let list: Vec<Value> = expressions
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({"id": format!("x{i}"), "key": key, "value": value}))
        .collect();
    let content: GraphContent = serde_json::from_value(json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            {"id": "expr", "name": "expr", "type": "expressionNode", "content": {"expressions": list, "passThrough": false}},
            {"id": "table", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "passThrough": true,
                "inputs": [{"id": "i0", "field": "price"}, {"id": "i1", "field": "day"}],
                "outputs": [{"id": "o0", "field": "tier"}],
                "rules": [
                    {"_id": "r0", "i0": "> 300", "i1": "> '2025-01-01'", "o0": "'gold'"},
                    {"_id": "r1", "i0": "[100..300]", "i1": "", "o0": "'silver'"},
                    {"_id": "r2", "i0": "", "i1": "", "o0": "'basic'"}
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
        Variable::Number(n) => json!(format!("#{}", n.normalize())),
        Variable::String(s) => json!(s.as_str()),
        Variable::Array(items) => Value::Array(items.borrow().iter().map(canon).collect()),
        Variable::Object(map) => Value::Object(
            map.borrow()
                .iter()
                .filter(|(_, v)| !matches!(v, Variable::Null))
                .map(|(k, v)| (k.to_string(), canon(v)))
                .collect(),
        ),
        Variable::Dynamic(d) => json!(d.to_string()),
    }
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

fn with_fixture(run: impl FnOnce(&Columns, &Columns, &Columns)) {
    let data = Data::new();
    let reference = Reference::new(&data);
    let when_days: Vec<i32> = data.when.iter().map(|v| *v as i32).collect();
    let foreign = Foreign::new(&data, &when_days);
    let reference_items = [
        ("amount", Column::new(Values::Dec(&reference.amount))),
        ("when", Column::new(reference.when.values())),
    ];
    let foreign_items = [
        ("amount", Column::new(Values::Decimal128(&foreign.amount))),
        ("when", Column::new(Values::Temporal(&foreign.when))),
    ];
    let reference_child = Column::new(Values::Struct {
        fields: &reference_items,
        len: data.amount.len(),
    });
    let foreign_child = Column::new(Values::Struct {
        fields: &foreign_items,
        len: data.amount.len(),
    });
    let expected = columns(
        [
            Values::Dec(&reference.price),
            Values::Dec(&reference.qty),
            Values::Dec(&reference.wide),
            Values::Dec(&reference.fine),
            Values::Dec(&reference.neg),
            reference.at.values(),
            reference.berlin.values(),
            reference.naive.values(),
            reference.day.values(),
            reference.clock.values(),
            Values::List {
                offsets: &data.offsets,
                child: Dictionary::Column(&reference_child),
            },
        ],
        &data,
        Some(&reference.wide_valid),
        true,
    );
    let values = || {
        [
            Values::Decimal128(&foreign.price),
            Values::Decimal128(&foreign.qty),
            Values::Decimal128(&foreign.wide),
            Values::Decimal128(&foreign.fine),
            Values::Decimal128(&foreign.neg),
            Values::Temporal(&foreign.at),
            Values::Temporal(&foreign.berlin),
            Values::Temporal(&foreign.naive),
            Values::Temporal(&foreign.day),
            Values::Temporal(&foreign.clock),
            Values::List {
                offsets: &data.offsets,
                child: Dictionary::Column(&foreign_child),
            },
        ]
    };
    let built = columns(values(), &data, None, true);
    let literal = columns(values(), &data, None, false);
    run(&expected, &built, &literal);
}

#[test]
fn decimal128_and_temporal_columns_materialise_like_their_text() {
    with_fixture(|expected, built, literal| {
        for row in 0..ROWS {
            let want = canon(&expected.row(row));
            assert_eq!(canon(&built.row(row)), want, "built row {row}");
            assert_eq!(canon(&literal.row(row)), want, "literal row {row}");
        }
        let sample = canon(&expected.row(0));
        assert_eq!(sample["at"].as_str().map(str::len), Some(24));
        assert!(sample["berlin"]
            .as_str()
            .is_some_and(|t| t.ends_with(":00")));
    });
}

#[test]
fn decimal128_and_temporal_columns_evaluate_like_scaled_and_text_columns() {
    with_fixture(|expected, built, literal| {
        for version in [EngineVersion::V2] {
            let decision = graph().with_version(version);
            let want = evaluate(&decision, expected);
            assert!(
                want.iter().all(|v| v.get("error").is_none()),
                "{:?}",
                want.iter().find(|v| v.get("error").is_some())
            );
            let got_built = evaluate(&decision, built);
            let got_literal = evaluate(&decision, literal);
            for row in 0..ROWS {
                assert_eq!(got_built[row], want[row], "built row {row}");
                assert_eq!(got_literal[row], want[row], "literal row {row}");
            }
        }
    });
}

#[test]
fn decimal128_and_temporal_columns_evaluate_like_version_one() {
    with_fixture(|expected, built, _| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let v1 = graph().with_version(EngineVersion::V1);
        let got = evaluate(&graph().with_version(EngineVersion::V2), built);
        for (row, got) in got.iter().enumerate() {
            let input = expected.row(row);
            let single = runtime
                .block_on(v1.evaluate_with_opts(input, options()))
                .map(|response| canon(&response.result))
                .unwrap_or_else(|_| json!({"error": true}));
            assert_eq!(got, &single, "row {row}");
        }
    });
}

#[test]
fn decimal128_values_out_of_range_read_as_null() {
    let values = [1i128 << 100, 5, i128::MAX];
    let decimals = Decimal128::new(&values, 0);
    let negative = Decimal128::new(&values, -30);
    let columns = Columns::new(3)
        .column("a", Column::new(Values::Decimal128(&decimals)))
        .column("b", Column::new(Values::Decimal128(&negative)));
    assert_eq!(canon(&columns.row(0)), json!({}));
    assert_eq!(canon(&columns.row(1)), json!({"a": "#5"}));
    assert_eq!(canon(&columns.row(2)), json!({}));
}

#[test]
fn temporal_columns_reject_unknown_zones_and_null_out_of_range_years() {
    assert!(Temporal::timestamp(&[0], TimeUnit::Second, Some("Mars/Olympus")).is_none());
    let days = [0, 3_000_000, -800_000, 2_932_896];
    let dates = Temporal::date32(&days);
    let offsets = [0i64, 1_000];
    let fixed = Temporal::timestamp(&offsets, TimeUnit::Millisecond, Some("-03:30")).unwrap();
    let columns = Columns::new(4)
        .column("d", Column::new(Values::Temporal(&dates)))
        .column("t", Column::new(Values::Temporal(&fixed)));
    assert_eq!(
        canon(&columns.row(0)),
        json!({"d": "1970-01-01", "t": "1969-12-31T20:30:00-03:30"})
    );
    assert_eq!(
        canon(&columns.row(1)),
        json!({"t": "1969-12-31T20:30:01-03:30"})
    );
    assert_eq!(canon(&columns.row(2)), json!({}));
    assert_eq!(canon(&columns.row(3)), json!({"d": "9999-12-31"}));
}

fn policy() -> zen_engine::Workspace {
    let property = |id: &str, name: &str, kind: &str| json!({"id": id, "name": name, "type": kind, "array": false, "optional": true});
    let expression = |id: &str, key: &str, value: &str| json!({"id": id, "type": "expression", "props": {"data": {"key": key, "value": value}}});
    let document = json!({"blocks": [
        {"id": "model", "type": "dataModel", "props": {"data": {"name": "order", "properties": [
            property("p1", "price", "number"),
            property("p2", "qty", "number"),
            property("p3", "placed", "string"),
            property("p4", "day", "string"),
        ]}}},
        expression("e1", "order.total", "order.price * order.qty"),
        expression("e2", "order.late", "order.placed != null and d(order.placed) > d('2024-01-01')"),
        expression("e3", "order.month", "d(order.day).month()"),
        expression("e4", "order.stamp", "order.placed"),
    ]});
    let mut ws = zen_engine::Workspace::new().with_version(EngineVersion::V2);
    ws.set_policy("policy", serde_json::from_value(document).unwrap());
    ws
}

#[test]
fn decimal128_and_temporal_columns_evaluate_policies_like_text_and_single_rows() {
    let data = Data::new();
    let reference = Reference::new(&data);
    let price = Decimal128::new(&data.price, 2);
    let qty = Decimal128::new(&data.qty, 0);
    let at = Temporal::timestamp(&data.at, TimeUnit::Millisecond, Some("UTC")).unwrap();
    let day = Temporal::date32(&data.day);
    let foreign = Columns::new(ROWS)
        .column("order.price", Column::new(Values::Decimal128(&price)))
        .column("order.qty", Column::new(Values::Decimal128(&qty)))
        .column(
            "order.placed",
            Column::with_validity(Values::Temporal(&at), &data.valid, 0),
        )
        .column("order.day", Column::new(Values::Temporal(&day)));
    let expected = Columns::new(ROWS)
        .column("order.price", Column::new(Values::Dec(&reference.price)))
        .column("order.qty", Column::new(Values::Dec(&reference.qty)))
        .column(
            "order.placed",
            Column::with_validity(reference.at.values(), &data.valid, 0),
        )
        .column("order.day", Column::new(reference.day.values()));
    let ws = policy();
    let path: Arc<str> = Arc::from("policy");
    let got = ws
        .evaluate_columns_pinned(&path, &[], &foreign, &options())
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
    assert!(filled > ROWS / 2);
}

fn passing(pass_through: bool, expressions: &[(&str, &str)]) -> Decision {
    let list: Vec<Value> = expressions
        .iter()
        .enumerate()
        .map(|(i, (key, value))| json!({"id": format!("x{i}"), "key": key, "value": value}))
        .collect();
    let content: GraphContent = serde_json::from_value(json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            {"id": "expr", "name": "expr", "type": "expressionNode", "content": {"expressions": list, "passThrough": pass_through}},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "expr", "type": "edge"},
            {"id": "e2", "sourceId": "expr", "targetId": "out", "type": "edge"}
        ]
    }))
    .unwrap();
    Decision::from(Arc::new(content)).with_version(EngineVersion::V2)
}

#[test]
fn unresolved_foreign_columns_evaluate_like_resolved_ones() {
    const N: usize = 200;
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let days: Vec<i32> = (0..N)
        .map(|i| match i % 9 {
            0 => 3_000_000,
            1 => -800_000,
            _ => 15_000 + rng.below(8_000) as i32,
        })
        .collect();
    let stamps: Vec<i64> = (0..N)
        .map(|i| match i % 11 {
            0 => i64::MAX / 2,
            _ => 1_600_000_000_000 + rng.below(200_000_000_000) as i64,
        })
        .collect();
    let amounts: Vec<i128> = (0..N)
        .map(|i| match i % 7 {
            0 => i128::MAX / 3,
            _ => rng.below(1_000_000) as i128 - 500_000,
        })
        .collect();
    let floats: Vec<f64> = (0..N)
        .map(|i| match i % 13 {
            0 => f64::NAN,
            _ => rng.below(100_000) as f64 / 100.0,
        })
        .collect();
    let mut valid = vec![0u64; N.div_ceil(64)];
    (0..N)
        .filter(|i| i % 5 != 2)
        .for_each(|i| valid[i / 64] |= 1 << (i % 64));
    let make = |builder: bool| {
        let day = Temporal::date32(&days);
        let at =
            Temporal::timestamp(&stamps, TimeUnit::Millisecond, Some("Europe/Berlin")).unwrap();
        let amount = Decimal128::new(&amounts, -3);
        let ratio = Float64::new(&floats);
        let list = [
            ("day", Column::new(Values::Temporal(&day))),
            (
                "at",
                Column::with_validity(Values::Temporal(&at), &valid, 0),
            ),
            (
                "amount",
                Column::with_validity(Values::Decimal128(&amount), &valid, 0),
            ),
            ("ratio", Column::new(Values::Float64(&ratio))),
        ];
        let columns = match builder {
            true => list
                .into_iter()
                .fold(Columns::new(N), |columns, (path, column)| {
                    columns.column(path, column)
                }),
            false => list
                .into_iter()
                .fold(Columns::new(N), |columns, (path, column)| {
                    columns.deferred(path, column)
                }),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let graphs = [
            passing(
                true,
                &[
                    ("year", "day == null ? 0 : d(day).year()"),
                    ("seen", "at != null"),
                ],
            ),
            passing(true, &[("fixed", "1 + 1")]),
            passing(false, &[("fixed", "1 + 1")]),
            passing(
                false,
                &[("hour", "at == null ? -1 : d(at).hour()"), ("day", "day")],
            ),
            passing(
                false,
                &[
                    ("double", "amount == null ? 0 : amount * 2"),
                    ("half", "ratio == null ? 0 : ratio / 2"),
                ],
            ),
            passing(true, &[("text", "string(day) + ' ' + string(at)")]),
        ];
        let mut results = Vec::new();
        for graph in &graphs {
            let output = runtime.block_on(graph.evaluate_columns(&columns, options()));
            results.push(
                (0..N)
                    .map(|row| match &output.errors[row] {
                        Some(_) => json!({"error": true}),
                        None => output.row(row).to_value(),
                    })
                    .collect::<Vec<Value>>(),
            );
        }
        results.push((0..N).map(|row| columns.row(row).to_value()).collect());
        results
    };
    let (resolved, unresolved) = (make(true), make(false));
    for (graph, (want, got)) in resolved.iter().zip(&unresolved).enumerate() {
        for row in 0..N {
            assert_eq!(got[row], want[row], "graph {graph} row {row}");
        }
    }
    let nulls = resolved[0]
        .iter()
        .filter(|row| row.get("day").is_none())
        .count();
    assert!(nulls > N / 9, "{nulls}");
}

#[test]
fn unresolved_foreign_columns_evaluate_policies_like_resolved_ones() {
    let data = Data::new();
    let days: Vec<i32> = data
        .day
        .iter()
        .enumerate()
        .map(|(i, day)| if i % 17 == 3 { 3_000_000 } else { *day })
        .collect();
    let make = |builder: bool| {
        let price = Decimal128::new(&data.price, 2);
        let qty = Decimal128::new(&data.qty, 0);
        let at = Temporal::timestamp(&data.at, TimeUnit::Millisecond, Some("UTC")).unwrap();
        let day = Temporal::date32(&days);
        let list = [
            ("order.price", Column::new(Values::Decimal128(&price))),
            ("order.qty", Column::new(Values::Decimal128(&qty))),
            (
                "order.placed",
                Column::with_validity(Values::Temporal(&at), &data.valid, 0),
            ),
            ("order.day", Column::new(Values::Temporal(&day))),
        ];
        let columns = match builder {
            true => list
                .into_iter()
                .fold(Columns::new(ROWS), |columns, (path, column)| {
                    columns.column(path, column)
                }),
            false => Columns {
                rows: ROWS,
                columns: list.to_vec(),
            },
        };
        let path: Arc<str> = Arc::from("policy");
        let output = policy()
            .evaluate_columns_pinned(&path, &[], &columns, &options())
            .unwrap();
        (0..ROWS)
            .map(|row| match &output.errors[row] {
                Some(_) => json!({"error": true}),
                None => output.row(row).to_value(),
            })
            .collect::<Vec<Value>>()
    };
    let (resolved, unresolved) = (make(true), make(false));
    let fine = resolved
        .iter()
        .filter(|row| row.get("error").is_none())
        .count();
    assert!(fine > ROWS / 2, "{fine}");
    for row in 0..ROWS {
        assert_eq!(unresolved[row], resolved[row], "row {row}");
    }
}

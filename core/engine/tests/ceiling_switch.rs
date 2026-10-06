mod support;

use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use support::test_data_root;
use zen_engine::model::{DecisionContent, GraphContent};
use zen_engine::{Decision, EvaluationOptions};
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;

struct Fixture {
    content: GraphContent,
    inputs: Vec<Value>,
}

impl Fixture {
    fn load(name: &str) -> Fixture {
        let path = PathBuf::from(test_data_root()).join("graphs").join(name);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let DecisionContent::Graph(graph) = serde_json::from_value(raw.clone()).unwrap() else {
            panic!("not a graph");
        };
        let inputs = raw["tests"].as_array().unwrap().iter().map(|t| t["input"].clone()).collect();
        Fixture {
            content: (*graph).clone(),
            inputs,
        }
    }

    fn typed_variants(&self) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        for base in &self.inputs {
            out.push(base.clone());
            let Some(object) = base.as_object() else {
                continue;
            };
            for (key, value) in object {
                let mut with = |replacement: Option<Value>| {
                    let mut copy = object.clone();
                    match replacement {
                        Some(v) => copy.insert(key.clone(), v),
                        None => copy.remove(key),
                    };
                    out.push(Value::Object(copy));
                };
                with(None);
                match value {
                    Value::Number(n) => {
                        for delta in ["1", "-1", "0", "1000000", "0.5"] {
                            let shifted: Option<Decimal> = n
                                .to_string()
                                .parse::<Decimal>()
                                .ok()
                                .zip(delta.parse::<Decimal>().ok())
                                .and_then(|(a, b)| a.checked_add(b));
                            if let Some(v) = shifted.and_then(|d| serde_json::from_str(&d.to_string()).ok()) {
                                with(Some(v));
                            }
                        }
                    }
                    Value::String(s) => {
                        with(Some(json!("")));
                        with(Some(json!(format!("{s}x"))));
                    }
                    Value::Bool(b) => with(Some(json!(!b))),
                    _ => {}
                }
            }
        }
        out.truncate(400);
        out
    }
}

enum Buf {
    Dec(Vec<Decimal>),
    Text(Vec<i32>, Vec<u8>),
    Bool(Vec<u64>),
    Any(Vec<Variable>),
}

struct Built {
    rows: usize,
    paths: Vec<String>,
    bufs: Vec<(Buf, Vec<u64>)>,
}

impl Built {
    fn flatten(value: &Value, prefix: &str, out: &mut Vec<(String, Value)>) -> bool {
        match value {
            Value::Object(map) => map.iter().all(|(key, child)| {
                !key.contains('.')
                    && Self::flatten(
                        child,
                        &match prefix.is_empty() {
                            true => key.clone(),
                            false => format!("{prefix}.{key}"),
                        },
                        out,
                    )
            }),
            Value::Null => true,
            other if !prefix.is_empty() => {
                out.push((prefix.to_string(), other.clone()));
                true
            }
            _ => false,
        }
    }

    fn new(rows: &[Value]) -> Option<Built> {
        let conflicts = |a: &str, b: &str| {
            a.strip_prefix(b).is_some_and(|r| r.starts_with('.')) || b.strip_prefix(a).is_some_and(|r| r.starts_with('.'))
        };
        let mut kept: Vec<Vec<(String, Value)>> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for value in rows {
            let mut entries = Vec::new();
            if !Self::flatten(value, "", &mut entries) {
                continue;
            }
            if entries.iter().any(|(p, _)| seen.iter().any(|s| conflicts(p, s))) {
                continue;
            }
            for (p, _) in &entries {
                if !seen.contains(p) {
                    seen.push(p.clone());
                }
            }
            kept.push(entries);
        }
        if kept.is_empty() {
            return None;
        }
        let rows: Vec<()> = vec![(); kept.len()];
        let mut paths: Vec<String> = Vec::new();
        let mut cells: Vec<Vec<Option<Value>>> = Vec::new();
        for (row, entries) in kept.into_iter().enumerate() {
            for (path, leaf) in entries {
                let at = match paths.iter().position(|p| *p == path) {
                    Some(at) => at,
                    None => {
                        paths.push(path);
                        cells.push(vec![None; rows.len()]);
                        cells.len() - 1
                    }
                };
                cells[at][row] = Some(leaf);
            }
        }
        let words = rows.len().div_ceil(64);
        let bufs = cells
            .into_iter()
            .map(|column| {
                let mut valid = vec![0u64; words];
                for (row, cell) in column.iter().enumerate() {
                    if cell.is_some() {
                        valid[row / 64] |= 1 << (row % 64);
                    }
                }
                let present: Vec<&Value> = column.iter().flatten().collect();
                let buf = if !present.is_empty() && present.iter().all(|v| v.is_number()) {
                    Buf::Dec(
                        column
                            .iter()
                            .map(|c| match c {
                                Some(Value::Number(n)) => n.to_string().parse().unwrap_or_default(),
                                _ => Decimal::ZERO,
                            })
                            .collect(),
                    )
                } else if !present.is_empty() && present.iter().all(|v| v.is_string()) {
                    let mut offsets = vec![0i32];
                    let mut data = Vec::new();
                    for cell in &column {
                        if let Some(Value::String(s)) = cell {
                            data.extend_from_slice(s.as_bytes());
                        }
                        offsets.push(data.len() as i32);
                    }
                    Buf::Text(offsets, data)
                } else if !present.is_empty() && present.iter().all(|v| v.is_boolean()) {
                    let mut bits = vec![0u64; words];
                    for (row, cell) in column.iter().enumerate() {
                        if let Some(Value::Bool(true)) = cell {
                            bits[row / 64] |= 1 << (row % 64);
                        }
                    }
                    Buf::Bool(bits)
                } else {
                    Buf::Any(
                        column
                            .iter()
                            .map(|c| c.as_ref().map_or(Variable::Null, |v| Variable::from(v.clone())))
                            .collect(),
                    )
                };
                (buf, valid)
            })
            .collect();
        Some(Built {
            rows: rows.len(),
            paths,
            bufs,
        })
    }

    fn columns(&self) -> Columns<'_> {
        let mut columns = Columns::new(self.rows);
        for (path, (buf, valid)) in self.paths.iter().zip(&self.bufs) {
            let values = match buf {
                Buf::Dec(v) => Values::Dec(v),
                Buf::Text(offsets, data) => Values::Utf8 { offsets, data },
                Buf::Bool(bits) => Values::Bool { bits, offset: 0 },
                Buf::Any(v) => Values::Any(v),
            };
            columns = columns.column(path, Column::with_validity(values, valid, 0));
        }
        columns
    }

    fn normalized(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let cleaned: serde_json::Map<String, Value> = map
                    .into_iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k, Self::normalized(v)))
                    .filter(|(_, v)| !matches!(v, Value::Object(m) if m.is_empty()))
                    .collect();
                Value::Object(cleaned)
            }
            other => other,
        }
    }
}

const GREEN: i32 = 0;
const AMBER: i32 = 1;
const RED: i32 = 2;
const FLAG_OFFSETS: [i32; 4] = [0, 5, 10, 13];
const FLAG_DATA: &str = "greenamberred";

struct Thresholds {
    llc_green: Decimal,
    llc_amber: Decimal,
    corp_green: Decimal,
    corp_amber: Decimal,
}

enum Arrays<'a> {
    Any(&'a [Variable]),
    Lens(&'a [u32]),
}

struct Inputs<'a> {
    rows: usize,
    kind: (&'a [i32], &'a [u8], &'a [u64]),
    turnover: (&'a [Decimal], &'a [u64]),
    secretaries: (Arrays<'a>, &'a [u64]),
    directors: (Arrays<'a>, &'a [u64]),
}

#[derive(Default)]
struct HandOutput {
    secretaries: Vec<i32>,
    secretaries_valid: Vec<u64>,
    directors: Vec<i32>,
    directors_valid: Vec<u64>,
    turnover: Vec<i32>,
    turnover_valid: Vec<u64>,
    pass: Vec<u64>,
    errors: Vec<u64>,
}

struct Hand;

impl Hand {
    fn thresholds() -> Thresholds {
        Thresholds {
            llc_green: Decimal::from(1_000_000),
            llc_amber: Decimal::from(200_000),
            corp_green: Decimal::from(10_000_000),
            corp_amber: Decimal::from(1_000_000),
        }
    }

    #[inline(always)]
    fn bit(bits: &[u64], row: usize) -> bool {
        bits[row >> 6] >> (row & 63) & 1 == 1
    }

    #[inline(always)]
    fn len(arrays: &Arrays, row: usize) -> Option<i64> {
        match arrays {
            Arrays::Lens(lens) => Some(lens[row] as i64),
            Arrays::Any(values) => match &values[row] {
                Variable::Array(a) => Some(a.borrow().len() as i64),
                Variable::String(s) => Some(s.chars().count() as i64),
                _ => None,
            },
        }
    }

    #[inline(always)]
    fn not_null(arrays: &Arrays, valid: &[u64], row: usize) -> bool {
        Self::bit(valid, row)
            && match arrays {
                Arrays::Lens(_) => true,
                Arrays::Any(values) => !matches!(values[row], Variable::Null),
            }
    }

    fn run<const P: u8>(input: &Inputs, t: &Thresholds, out: &mut HandOutput) {
        let rows = input.rows;
        let words = rows.div_ceil(64);
        out.secretaries.resize(rows, -1);
        out.directors.resize(rows, -1);
        out.turnover.resize(rows, -1);
        for v in [&mut out.secretaries_valid, &mut out.directors_valid, &mut out.turnover_valid, &mut out.pass, &mut out.errors] {
            v.clear();
            v.resize(words, 0);
        }
        let (offsets, data, kind_valid) = input.kind;
        let (turnover, turnover_valid) = input.turnover;
        let (secretaries, sec_valid) = (&input.secretaries.0, input.secretaries.1);
        let (directors, dir_valid) = (&input.directors.0, input.directors.1);
        for w in 0..words {
            let base = w * 64;
            let width = (rows - base).min(64);
            let mut llc = 0u64;
            let mut corp = 0u64;
            let kv = kind_valid[w];
            for i in 0..width {
                let row = base + i;
                let (a, b) = (offsets[row] as usize, offsets[row + 1] as usize);
                let s = &data[a..b];
                llc |= (((kv >> i & 1 == 1) & (s == b"LLC")) as u64) << i;
                corp |= (((kv >> i & 1 == 1) & (s == b"Corporation")) as u64) << i;
            }
            out.pass[w] = !(llc | corp) & if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
            let routed = llc | corp;
            out.turnover_valid[w] = routed;
            if P & 1 == 0 {
                continue;
            }
            let tv = turnover_valid[w];
            let mut m = routed;
            while m != 0 {
                let i = m.trailing_zeros() as usize;
                m &= m - 1;
                let row = base + i;
                let (green, amber) = match llc >> i & 1 == 1 {
                    true => (&t.llc_green, &t.llc_amber),
                    false => (&t.corp_green, &t.corp_amber),
                };
                out.turnover[row] = match tv >> i & 1 == 1 {
                    false => RED,
                    true => {
                        let v = &turnover[row];
                        match (v > green, v >= amber && v <= green) {
                            (true, _) => GREEN,
                            (_, true) => AMBER,
                            _ => RED,
                        }
                    }
                };
            }
            if P & 2 == 0 {
                continue;
            }
            let mut sec = 0u64;
            let mut m = routed;
            while m != 0 {
                let i = m.trailing_zeros() as usize;
                m &= m - 1;
                let row = base + i;
                if !Self::not_null(secretaries, sec_valid, row) {
                    continue;
                }
                sec |= 1 << i;
                let Some(n) = Self::len(secretaries, row) else {
                    out.errors[w] |= 1 << i;
                    continue;
                };
                out.secretaries[row] = match (llc >> i & 1 == 1, n > 2, n > 1) {
                    (_, true, _) => GREEN,
                    (true, _, true) => AMBER,
                    _ => RED,
                };
            }
            out.secretaries_valid[w] = sec;
            let mut dir = 0u64;
            let mut m = llc;
            while m != 0 {
                let i = m.trailing_zeros() as usize;
                m &= m - 1;
                let row = base + i;
                if !Self::not_null(directors, dir_valid, row) {
                    continue;
                }
                dir |= 1 << i;
                let Some(n) = Self::len(directors, row) else {
                    out.errors[w] |= 1 << i;
                    continue;
                };
                out.directors[row] = if n > 2 { GREEN } else { RED };
            }
            out.directors_valid[w] = dir;
        }
    }
}

struct Inputs2 {
    sec_lens: Vec<u32>,
    dir_lens: Vec<u32>,
}

#[tokio::test]
#[ignore]
async fn ceiling_multi_switch() {
    let fixture = Fixture::load("multi-switch.json");
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
    compiled.compile();
    assert!(matches!(compiled.compiled_verdict(), Some(Ok(()))));
    let walker = compiled.interpreted();
    let variants = fixture.typed_variants();
    let inputs: Vec<Value> = (0..rows).map(|i| variants[i % variants.len()].clone()).collect();
    let built = Built::new(&inputs).unwrap();
    let columns = built.columns();
    println!("paths {:?}", built.paths);
    let find = |name: &str| built.paths.iter().position(|p| p == name).unwrap();
    let (kind_offsets, kind_data, kind_valid) = match &built.bufs[find("company.type")] {
        (Buf::Text(o, d), v) => (o.as_slice(), d.as_slice(), v.as_slice()),
        _ => panic!("type"),
    };
    let (turnover, turnover_valid) = match &built.bufs[find("company.turnover")] {
        (Buf::Dec(d), v) => (d.as_slice(), v.as_slice()),
        _ => panic!("turnover"),
    };
    let any = |name: &str| match &built.bufs[find(name)] {
        (Buf::Any(a), v) => (a.as_slice(), v.as_slice()),
        _ => panic!("{name}"),
    };
    let (sec_any, sec_valid) = any("company.secretaries");
    let (dir_any, dir_valid) = any("company.directors");
    let lens = |values: &[Variable]| -> Vec<u32> {
        values
            .iter()
            .map(|v| match v {
                Variable::Array(a) => a.borrow().len() as u32,
                _ => 0,
            })
            .collect()
    };
    let typed_lists = Inputs2 {
        sec_lens: lens(sec_any),
        dir_lens: lens(dir_any),
    };
    let input_any = Inputs {
        rows: built.rows,
        kind: (kind_offsets, kind_data, kind_valid),
        turnover: (turnover, turnover_valid),
        secretaries: (Arrays::Any(sec_any), sec_valid),
        directors: (Arrays::Any(dir_any), dir_valid),
    };
    let input_list = Inputs {
        rows: built.rows,
        kind: (kind_offsets, kind_data, kind_valid),
        turnover: (turnover, turnover_valid),
        secretaries: (Arrays::Lens(&typed_lists.sec_lens), sec_valid),
        directors: (Arrays::Lens(&typed_lists.dir_lens), dir_valid),
    };
    let thresholds = Hand::thresholds();
    let mut out = HandOutput::default();
    Hand::run::<3>(&input_any, &thresholds, &mut out);

    let flag_dict = Dictionary::Text {
        offsets: &FLAG_OFFSETS,
        data: FLAG_DATA,
    };
    let out_columns: Vec<(&str, Column)> = {
        let mut cols: Vec<(&str, Column)> = Vec::new();
        cols.push((
            "flag.turnover",
            Column::with_validity(Values::Dict { keys: &out.turnover, values: flag_dict }, &out.turnover_valid, 0),
        ));
        cols.push((
            "flag.secretaries",
            Column::with_validity(Values::Dict { keys: &out.secretaries, values: flag_dict }, &out.secretaries_valid, 0),
        ));
        cols.push((
            "flag.directors",
            Column::with_validity(Values::Dict { keys: &out.directors, values: flag_dict }, &out.directors_valid, 0),
        ));
        cols
    };
    let pass_valid: Vec<Vec<u64>> = built
        .bufs
        .iter()
        .map(|(_, valid)| valid.iter().zip(&out.pass).map(|(a, b)| a & b).collect())
        .collect();
    let pass_columns: Vec<(&str, Column)> = columns
        .columns
        .iter()
        .zip(&pass_valid)
        .map(|((path, column), valid)| (*path, Column::with_validity(column.values, valid, 0)))
        .collect();
    let hand_row = |row: usize| -> Variable {
        let object = Variable::empty_object();
        for (path, column) in pass_columns.iter().chain(&out_columns) {
            if column.valid(row) {
                object.dot_insert(path, column.variable(row));
            }
        }
        object
    };

    let engine_out = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
    let mut mismatches = 0usize;
    let mut order_mismatches = 0usize;
    for row in 0..built.rows {
        let want = walker.evaluate(columns.row(row)).await.unwrap().result;
        assert!(out.errors[row / 64] >> (row % 64) & 1 == 0);
        let got = hand_row(row);
        let engine = engine_out.row(row);
        if got.to_value() != want.to_value() {
            mismatches += 1;
            if mismatches < 5 {
                println!("row {row}: walker {} hand {}", want.to_value(), got.to_value());
            }
        }
        if serde_json::to_string(&got).unwrap() != serde_json::to_string(&want).unwrap() {
            order_mismatches += 1;
            if order_mismatches < 3 {
                println!("order row {row}: walker {} hand {}", serde_json::to_string(&want).unwrap(), serde_json::to_string(&got).unwrap());
            }
        }
        assert!(engine_out.errors[row].is_none());
        assert_eq!(Built::normalized(engine.to_value()), Built::normalized(want.to_value()), "engine row {row}");
    }
    println!("parity: {} rows, {mismatches} value mismatches, {order_mismatches} key-order mismatches", built.rows);
    assert_eq!(mismatches, 0);

    let n = built.rows as f64;
    let reps = 2000;
    let mut best = [f64::MAX; 4];
    let options = EvaluationOptions::default();
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..reps {
            Hand::run::<3>(black_box(&input_any), &thresholds, &mut out);
            black_box(&out);
        }
        best[0] = best[0].min(start.elapsed().as_nanos() as f64 / (n * reps as f64));
        let start = Instant::now();
        for _ in 0..reps {
            Hand::run::<3>(black_box(&input_list), &thresholds, &mut out);
            black_box(&out);
        }
        best[1] = best[1].min(start.elapsed().as_nanos() as f64 / (n * reps as f64));
        let start = Instant::now();
        for _ in 0..reps {
            let mut fresh = HandOutput::default();
            Hand::run::<3>(black_box(&input_any), &thresholds, &mut fresh);
            let pass: Vec<Vec<u64>> = built
                .bufs
                .iter()
                .map(|(_, valid)| valid.iter().zip(&fresh.pass).map(|(a, b)| a & b).collect())
                .collect();
            black_box((&fresh, &pass));
        }
        best[2] = best[2].min(start.elapsed().as_nanos() as f64 / (n * reps as f64));
        let start = Instant::now();
        for _ in 0..50 {
            black_box(compiled.evaluate_columns(black_box(&columns), options).await);
        }
        best[3] = best[3].min(start.elapsed().as_nanos() as f64 / (n * 50.0));
    }
    let mut phases = [f64::MAX; 2];
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..reps {
            Hand::run::<0>(black_box(&input_any), &thresholds, &mut out);
            black_box(&out);
        }
        phases[0] = phases[0].min(start.elapsed().as_nanos() as f64 / (n * reps as f64));
        let start = Instant::now();
        for _ in 0..reps {
            Hand::run::<1>(black_box(&input_any), &thresholds, &mut out);
            black_box(&out);
        }
        phases[1] = phases[1].min(start.elapsed().as_nanos() as f64 / (n * reps as f64));
    }
    println!("phases: classify+init {:.2} | +turnover tables {:.2} | +len tables (full) {:.2} ns/row", phases[0], phases[1], best[0]);
    println!(
        "hand (Any arrays, reused buffers) {:.2} ns/row | hand (typed list lens) {:.2} ns/row | hand (fresh alloc + pass validity) {:.2} ns/row | evaluate_columns {:.1} ns/row",
        best[0], best[1], best[2], best[3]
    );
    if let Ok(seconds) = std::env::var("BENCH_PROFILE") {
        let until = Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(5));
        while Instant::now() < until {
            black_box(compiled.evaluate_columns(&columns, options).await);
        }
    }
}

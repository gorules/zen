mod support;

use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use support::test_data_root;
use zen_engine::model::DecisionContent;
use zen_engine::{Decision, EvaluationOptions};
use zen_expression::Variable;

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
        let mut kept: Vec<Vec<(String, Value)>> = Vec::new();
        for value in rows {
            let mut entries = Vec::new();
            if !Self::flatten(value, "", &mut entries) {
                continue;
            }
            kept.push(entries);
        }
        let count = kept.len();
        let mut paths: Vec<String> = Vec::new();
        let mut cells: Vec<Vec<Option<Value>>> = Vec::new();
        for (row, entries) in kept.into_iter().enumerate() {
            for (path, leaf) in entries {
                let at = match paths.iter().position(|p| *p == path) {
                    Some(at) => at,
                    None => {
                        paths.push(path);
                        cells.push(vec![None; count]);
                        cells.len() - 1
                    }
                };
                cells[at][row] = Some(leaf);
            }
        }
        let words = count.div_ceil(64);
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
        Some(Built { rows: count, paths, bufs })
    }

    fn columns(&self) -> zen_expression::lane::Columns<'_> {
        use zen_expression::lane::{Column, Columns, Values};
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

    fn dec(&self, path: &str) -> Num<'_> {
        let at = self.paths.iter().position(|p| p == path).unwrap();
        match &self.bufs[at] {
            (Buf::Dec(v), valid) => Num { values: v, valid },
            _ => panic!("{path} not decimal"),
        }
    }

    fn bool(&self, path: &str) -> Flag<'_> {
        let at = self.paths.iter().position(|p| p == path).unwrap();
        match &self.bufs[at] {
            (Buf::Bool(bits), valid) => Flag { bits, valid },
            _ => panic!("{path} not bool"),
        }
    }
}

struct Num<'a> {
    values: &'a [Decimal],
    valid: &'a [u64],
}

struct Flag<'a> {
    bits: &'a [u64],
    valid: &'a [u64],
}

const POW10: [i128; 29] = {
    let mut p = [1i128; 29];
    let mut i = 1;
    while i < 29 {
        p[i] = p[i - 1] * 10;
        i += 1;
    }
    p
};

impl Num<'_> {
    #[inline(always)]
    fn scaled(d: &Decimal, c: i64) -> (i128, i128) {
        (d.mantissa(), c as i128 * POW10[d.scale() as usize])
    }

    #[inline(always)]
    fn integral(d: &Decimal) -> bool {
        d.scale() == 0 || d.mantissa() % POW10[d.scale() as usize] == 0
    }

    #[inline(always)]
    fn small(d: &Decimal) -> Option<i64> {
        match d.scale() == 0 {
            true => i64::try_from(d.mantissa()).ok(),
            false => None,
        }
    }

    fn above<const N: usize>(&self, thresholds: [i64; N], strict: bool, out: &mut [Vec<u64>; N]) {
        for (w, word) in self.values.chunks(64).enumerate() {
            let mut acc = [0u64; N];
            for (i, d) in word.iter().enumerate() {
                match Self::small(d) {
                    Some(v) => {
                        for k in 0..N {
                            let hit = if strict { v > thresholds[k] } else { v >= thresholds[k] };
                            acc[k] |= (hit as u64) << i;
                        }
                    }
                    None => {
                        for k in 0..N {
                            let (m, c) = Self::scaled(d, thresholds[k]);
                            let hit = if strict { m > c } else { m >= c };
                            acc[k] |= (hit as u64) << i;
                        }
                    }
                }
            }
            for k in 0..N {
                out[k][w] = acc[k] & self.valid[w];
            }
        }
    }

    fn below<const N: usize>(&self, thresholds: [i64; N], out: &mut [Vec<u64>; N]) {
        for (w, word) in self.values.chunks(64).enumerate() {
            let mut acc = [0u64; N];
            for (i, d) in word.iter().enumerate() {
                for k in 0..N {
                    let (m, c) = Self::scaled(d, thresholds[k]);
                    acc[k] |= ((m < c) as u64) << i;
                }
            }
            for k in 0..N {
                out[k][w] = acc[k] & self.valid[w];
            }
        }
    }

    fn fractional(&self, err: &mut [u64]) {
        for (w, word) in self.values.chunks(64).enumerate() {
            let mut acc = 0u64;
            for (i, d) in word.iter().enumerate() {
                acc |= (!Self::integral(d) as u64) << i;
            }
            err[w] |= acc & self.valid[w];
        }
    }
}

impl Flag<'_> {
    fn truthy(&self, w: usize) -> u64 {
        self.bits[w] & self.valid[w]
    }
}

struct Inputs<'a> {
    rows: usize,
    count: Num<'a>,
    high: Flag<'a>,
    variant: Flag<'a>,
    words: Num<'a>,
    bullets: Flag<'a>,
    specs: Flag<'a>,
    density: Num<'a>,
    rank: Num<'a>,
    stock: Num<'a>,
    ship: Num<'a>,
}

struct Preds {
    c: [Vec<u64>; 4],
    wc: [Vec<u64>; 4],
    kd_gt: [Vec<u64>; 3],
    kd_lt: [Vec<u64>; 2],
    sl: [Vec<u64>; 4],
    cr: [Vec<u64>; 3],
}

struct Out {
    image: Vec<u8>,
    desc: Vec<u8>,
    total: Vec<Decimal>,
    category: Vec<u8>,
    errors: Vec<u64>,
}

const IMG_SCORE: [i64; 5] = [30, 25, 20, 10, 0];
const IMG_CAT: [&str; 5] = ["excellent", "good", "average", "poor", "missing"];
const DESC: [[i64; 4]; 5] = [[30, 20, 10, 10], [25, 15, 8, 8], [15, 10, 5, 5], [10, 5, 3, 3], [0, 0, 0, 0]];
const DESC_CAT: [&str; 5] = ["excellent", "good", "average", "poor", "missing"];
const OVERALL: [&str; 5] = ["excellent", "good", "average", "poor", "unacceptable"];

impl<'a> Inputs<'a> {
    fn new(built: &'a Built) -> Self {
        Inputs {
            rows: built.rows,
            count: built.dec("listing.images.count"),
            high: built.bool("listing.images.highResolution"),
            variant: built.bool("listing.images.hasVariantImages"),
            words: built.dec("listing.description.wordCount"),
            bullets: built.bool("listing.description.hasBulletPoints"),
            specs: built.bool("listing.description.hasSpecifications"),
            density: built.dec("listing.description.keywordDensity"),
            rank: built.dec("listing.pricing.competitiveRank"),
            stock: built.dec("listing.inventory.stockLevel"),
            ship: built.dec("listing.inventory.daysToShip"),
        }
    }

    #[inline(always)]
    fn fixed(d: &Decimal) -> i128 {
        let scale = d.scale() as usize;
        match scale <= 8 {
            true => d.mantissa() * POW10[8 - scale],
            false => (d.round_dp(8)).mantissa(),
        }
    }

    #[inline(always)]
    fn bit(words: &[u64], r: usize) -> bool {
        (words[r >> 6] >> (r & 63)) & 1 == 1
    }

    fn rows(&self) -> Out {
        let n = self.rows;
        let errors = self.validate();
        let k = POW10[8];
        let mut image = Vec::with_capacity(n);
        let mut desc = Vec::with_capacity(n);
        let mut total = Vec::with_capacity(n);
        let mut category = Vec::with_capacity(n);
        for r in 0..n {
            let has = |v: &[u64]| Self::bit(v, r);
            let flag = |f: &Flag| Self::bit(f.bits, r) & Self::bit(f.valid, r);
            let num = |x: &Num| has(x.valid).then(|| Self::fixed(&x.values[r]));
            let count = num(&self.count);
            let ge = |v: Option<i128>, c: i128| v.is_some_and(|v| v >= c * k);
            let gt = |v: Option<i128>, c: i128| v.is_some_and(|v| v > c * k);
            let lt = |v: Option<i128>, c: i128| v.is_some_and(|v| v < c * k);
            let (hr, hv) = (flag(&self.high), flag(&self.variant));
            let a: u8 = if ge(count, 5) && hr && hv {
                0
            } else if ge(count, 3) && hr {
                1
            } else if ge(count, 2) && hr {
                2
            } else if ge(count, 1) {
                3
            } else {
                4
            };
            let (wc, kd, sl, cr) = (num(&self.words), num(&self.density), num(&self.stock), num(&self.rank));
            let (bp, sp) = (flag(&self.bullets), flag(&self.specs));
            let b: u8 = if gt(wc, 300) && bp && sp && gt(kd, 2) && lt(kd, 5) && gt(sl, 20) && lt(cr, 3) {
                0
            } else if gt(wc, 200) && bp && gt(kd, 1) && lt(kd, 6) && gt(sl, 10) && lt(cr, 5) {
                1
            } else if gt(wc, 100) && gt(kd, 0) && gt(sl, 5) && lt(cr, 10) {
                2
            } else if gt(wc, 50) && gt(sl, 0) {
                3
            } else {
                4
            };
            let d = DESC[b as usize];
            let s = IMG_SCORE[a as usize] + d[0] + d[1] + d[2] + d[3];
            image.push(a);
            desc.push(b);
            category.push(Self::overall(s));
            total.push(Decimal::from(s));
        }
        Out { image, desc, total, category, errors }
    }

    #[inline(always)]
    fn first(masks: &[u64], i: usize) -> u8 {
        let mut code = masks.len() as u8;
        for k in (0..masks.len()).rev() {
            if (masks[k] >> i) & 1 == 1 {
                code = k as u8;
            }
        }
        code
    }

    #[inline(always)]
    fn overall(total: i64) -> u8 {
        match total {
            t if t >= 80 => 0,
            t if t >= 60 => 1,
            t if t >= 40 => 2,
            t if t >= 20 => 3,
            _ => 4,
        }
    }

    fn run(&self, decimal_total: bool) -> Out {
        let errors = self.validate();
        let preds = self.predicates();
        self.select(errors, &preds, decimal_total)
    }

    fn validate(&self) -> Vec<u64> {
        let mut errors = vec![0u64; self.rows.div_ceil(64)];
        for column in [&self.count, &self.words, &self.rank, &self.stock, &self.ship] {
            column.fractional(&mut errors);
        }
        errors
    }

    fn predicates(&self) -> Preds {
        let words = self.rows.div_ceil(64);
        let z = || vec![0u64; words];
        let mut c = [z(), z(), z(), z()];
        self.count.above([5, 3, 2, 1], false, &mut c);
        let mut wc = [z(), z(), z(), z()];
        self.words.above([300, 200, 100, 50], true, &mut wc);
        let mut kd_gt = [z(), z(), z()];
        self.density.above([2, 1, 0], true, &mut kd_gt);
        let mut kd_lt = [z(), z()];
        self.density.below([5, 6], &mut kd_lt);
        let mut sl = [z(), z(), z(), z()];
        self.stock.above([20, 10, 5, 0], true, &mut sl);
        let mut cr = [z(), z(), z()];
        self.rank.below([3, 5, 10], &mut cr);
        Preds { c, wc, kd_gt, kd_lt, sl, cr }
    }

    fn select(&self, errors: Vec<u64>, p: &Preds, decimal_total: bool) -> Out {
        let n = self.rows;
        let words = n.div_ceil(64);
        let Preds { c, wc, kd_gt, kd_lt, sl, cr } = p;
        let mut image = vec![0u8; n];
        let mut desc = vec![0u8; n];
        let mut total = Vec::with_capacity(n);
        let mut category = vec![0u8; n];
        for w in 0..words {
            let (hr, hv) = (self.high.truthy(w), self.variant.truthy(w));
            let (bp, sp) = (self.bullets.truthy(w), self.specs.truthy(w));
            let img = [c[0][w] & hr & hv, c[1][w] & hr, c[2][w] & hr, c[3][w]];
            let des = [
                wc[0][w] & bp & sp & kd_gt[0][w] & kd_lt[0][w] & sl[0][w] & cr[0][w],
                wc[1][w] & bp & kd_gt[1][w] & kd_lt[1][w] & sl[1][w] & cr[1][w],
                wc[2][w] & kd_gt[2][w] & sl[2][w] & cr[2][w],
                wc[3][w] & sl[3][w],
            ];
            let base = w * 64;
            let end = (base + 64).min(n);
            for i in 0..end - base {
                let (a, b) = (Self::first(&img, i), Self::first(&des, i));
                image[base + i] = a;
                desc[base + i] = b;
                let d = DESC[b as usize];
                let sum = match decimal_total {
                    true => {
                        let s = Decimal::from(IMG_SCORE[a as usize])
                            + Decimal::from(d[0])
                            + Decimal::from(d[1])
                            + Decimal::from(d[2])
                            + Decimal::from(d[3]);
                        category[base + i] = match s {
                            s if s >= Decimal::from(80) => 0,
                            s if s >= Decimal::from(60) => 1,
                            s if s >= Decimal::from(40) => 2,
                            s if s >= Decimal::from(20) => 3,
                            _ => 4,
                        };
                        s
                    }
                    false => {
                        let s = IMG_SCORE[a as usize] + d[0] + d[1] + d[2] + d[3];
                        category[base + i] = Self::overall(s);
                        Decimal::from(s)
                    }
                };
                total.push(sum);
            }
        }
        Out { image, desc, total, category, errors }
    }
}

impl Out {
    fn num(d: Decimal) -> Value {
        serde_json::from_str(&d.normalize().to_string()).unwrap()
    }

    fn row(&self, r: usize) -> Option<Value> {
        if (self.errors[r / 64] >> (r % 64)) & 1 == 1 {
            return None;
        }
        let (a, b) = (self.image[r] as usize, self.desc[r] as usize);
        let d = DESC[b];
        Some(json!({
            "totalScore": Self::num(self.total[r]),
            "overallScoreCategory": OVERALL[self.category[r] as usize],
            "maximumPossibleScore": Self::num(Decimal::from(100)),
            "breakdown": {
                "imageQuality": { "score": Self::num(Decimal::from(IMG_SCORE[a])), "category": IMG_CAT[a] },
                "descriptionCompleteness": { "score": Self::num(Decimal::from(d[0])), "category": DESC_CAT[b] },
                "keywordOptimization": { "score": Self::num(Decimal::from(d[1])) },
                "inventoryAvailability": { "score": Self::num(Decimal::from(d[2])) },
                "pricingCompetitiveness": { "score": Self::num(Decimal::from(d[3])) },
            }
        }))
    }
}

struct Fixture;

impl Fixture {
    fn load() -> (zen_engine::model::GraphContent, Vec<Value>) {
        let path = PathBuf::from(test_data_root()).join("graphs/product-listing-scoring.json");
        let raw: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let DecisionContent::Graph(graph) = serde_json::from_value(raw.clone()).unwrap() else {
            panic!("not a graph");
        };
        let inputs: Vec<Value> = raw["tests"].as_array().unwrap().iter().map(|t| t["input"].clone()).collect();
        let mut out = Vec::new();
        for base in &inputs {
            out.push(base.clone());
            for (key, value) in base.as_object().unwrap() {
                let mut copy = base.as_object().unwrap().clone();
                copy.remove(key);
                out.push(Value::Object(copy));
                if let Value::Bool(b) = value {
                    let mut copy = base.as_object().unwrap().clone();
                    copy.insert(key.clone(), json!(!b));
                    out.push(Value::Object(copy));
                }
            }
        }
        ((*graph).clone(), out)
    }

    fn rich() -> Vec<Value> {
        let mut out = Vec::new();
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for _ in 0..400 {
            let mut num = |m: u64, frac: bool| -> Value {
                match next(9) {
                    0 => Value::Null,
                    1 if frac => json!(next(m) as f64 / 10.0),
                    _ => json!(next(m)),
                }
            };
            let count = num(8, false);
            let wc = num(400, false);
            let kd = num(80, true);
            let cr = num(12, false);
            let sl = num(30, false);
            let mut b = || match next(3) {
                0 => Value::Null,
                1 => json!(true),
                _ => json!(false),
            };
            out.push(json!({"listing": {
                "images": {"count": count, "highResolution": b(), "hasVariantImages": b()},
                "description": {"wordCount": wc, "hasBulletPoints": b(), "hasSpecifications": b(), "keywordDensity": kd},
                "pricing": {"competitiveRank": cr, "hasDiscount": b()},
                "inventory": {"stockLevel": sl, "daysToShip": 2}
            }}));
        }
        out
    }
}

#[tokio::test]
#[ignore]
async fn ceiling_product_listing_scoring() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let (content, variants) = Fixture::load();
    let mut compiled = Decision::from(Arc::new(content));
    compiled.compile();
    assert!(matches!(compiled.compiled_verdict(), Some(Ok(()))));
    let walker = compiled.interpreted();
    let options = EvaluationOptions::default();
    let sets: Vec<(&str, Vec<Value>)> = vec![
        ("harness", (0..rows).map(|i| variants[i % 2].clone()).collect()),
        ("rich", {
            let rich = Fixture::rich();
            (0..rows).map(|i| rich[i % rich.len()].clone()).collect()
        }),
    ];
    for (label, inputs) in &sets {
        let built = Built::new(inputs).unwrap();
        assert_eq!(built.rows, rows);
        let columns = built.columns();
        let ins = Inputs::new(&built);
        let hand = ins.run(true);
        let alt = ins.rows();
        let engine = compiled.evaluate_columns(&columns, options).await;
        let mut mismatches = 0;
        for r in 0..rows {
            let want = match walker.evaluate(columns.row(r)).await {
                Ok(v) => Some(Built::normalized(v.result.to_value())),
                Err(_) => None,
            };
            let eng = match &engine.errors[r] {
                Some(_) => None,
                None => Some(Built::normalized(engine.row(r).to_value())),
            };
            let got = hand.row(r);
            if alt.row(r) != want {
                mismatches += 1;
            }
            if got != want || eng != want {
                mismatches += 1;
                if mismatches < 5 {
                    println!("row {r} {}\n walker {want:?}\n hand   {got:?}\n engine {eng:?}", columns.row(r).to_value());
                }
            }
        }
        println!("{label}: parity mismatches {mismatches}/{rows}");
        assert_eq!(mismatches, 0);

        if *label == "harness" {
            use zen_expression::lane::Values;
            for (path, column) in &engine.columns {
                let kind = match column.column().values {
                    Values::Dec(_) => "Dec",
                    Values::Scaled { .. } => "Scaled",
                    Values::I64(_) => "I64",
                    Values::Text { .. } | Values::Utf8 { .. } | Values::Strs(_) | Values::LargeUtf8 { .. } => "Text",
                    Values::Dict { .. } => "Dict",
                    Values::Any(_) => "Any",
                    _ => "other",
                };
                println!("engine output column {path}: {kind}");
            }
        }
        let mut phases = [f64::MAX; 3];
        for _ in 0..9 {
            let ins = Inputs::new(&built);
            let t = Instant::now();
            let errors = std::hint::black_box(ins.validate());
            phases[0] = phases[0].min(t.elapsed().as_nanos() as f64 / rows as f64);
            let t = Instant::now();
            let preds = std::hint::black_box(ins.predicates());
            phases[1] = phases[1].min(t.elapsed().as_nanos() as f64 / rows as f64);
            let t = Instant::now();
            let out = std::hint::black_box(ins.select(errors, &preds, false));
            phases[2] = phases[2].min(t.elapsed().as_nanos() as f64 / rows as f64);
            drop(out);
        }
        println!("{label}: hand phases validate {:.2} predicates {:.2} select+outputs {:.2} ns/row", phases[0], phases[1], phases[2]);
        let mut best = [f64::MAX; 5];
        for _ in 0..9 {
            let t = Instant::now();
            for c in (0..rows.min(256)).map(|r| columns.row(r)) {
                let _ = walker.evaluate(c).await;
            }
            best[0] = best[0].min(t.elapsed().as_nanos() as f64 / rows.min(256) as f64);
            let t = Instant::now();
            let out = compiled.evaluate_columns(&columns, options).await;
            std::hint::black_box(&out);
            drop(out);
            best[1] = best[1].min(t.elapsed().as_nanos() as f64 / rows as f64);
            let t = Instant::now();
            let out = Inputs::new(&built).run(true);
            std::hint::black_box(&out);
            drop(out);
            best[2] = best[2].min(t.elapsed().as_nanos() as f64 / rows as f64);
            let t = Instant::now();
            let out = Inputs::new(&built).run(false);
            std::hint::black_box(&out);
            drop(out);
            best[3] = best[3].min(t.elapsed().as_nanos() as f64 / rows as f64);
            let t = Instant::now();
            let out = Inputs::new(&built).rows();
            std::hint::black_box(&out);
            drop(out);
            best[4] = best[4].min(t.elapsed().as_nanos() as f64 / rows as f64);
        }
        println!("{label}: hand row-wise {:.2} ns/row ({:.0}x)", best[4], best[0] / best[4]);
        println!(
            "{label}: walker {:.0} ns/row | engine {:.1} ns/row ({:.1}x) | hand decimal {:.2} ns/row ({:.0}x) | hand int {:.2} ns/row ({:.0}x)",
            best[0], best[1], best[0] / best[1], best[2], best[0] / best[2], best[3], best[0] / best[3]
        );
    }
    if let Ok(seconds) = std::env::var("BENCH_PROFILE") {
        let built = Built::new(&sets[0].1).unwrap();
        let columns = built.columns();
        let until = Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(5));
        while Instant::now() < until {
            let _ = compiled.evaluate_columns(&columns, options).await;
        }
    }
}

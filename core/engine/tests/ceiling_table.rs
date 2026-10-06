use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Arc;
use zen_engine::model::{DecisionContent, GraphContent};
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

    fn at(&self, path: &str) -> Option<&(Buf, Vec<u64>)> {
        self.paths.iter().position(|p| p == path).map(|i| &self.bufs[i])
    }
}

struct Fixture {
    content: GraphContent,
    inputs: Vec<Value>,
}

impl Fixture {
    fn load() -> Fixture {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/graphs/traffic-violation-penalty-calculator.json");
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let DecisionContent::Graph(graph) = serde_json::from_value(raw.clone()).unwrap() else {
            panic!("graph expected");
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

    fn random(rows: usize) -> Vec<Value> {
        let types = [
            "speeding",
            "running_red_light",
            "illegal_turn",
            "dui",
            "driving_without_license",
            "reckless_driving",
            "parking",
            "",
            "speedingx",
            "Speeding",
        ];
        let speeds = ["10", "15", "15.5", "16", "30", "30.01", "31", "0", "-1", "45.25"];
        let prevs = ["0", "1", "2", "2.5", "3", "3.5", "4", "10"];
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        (0..rows)
            .map(|_| {
                let mut violation = serde_json::Map::new();
                if next(12) > 0 {
                    violation.insert("type".into(), json!(types[next(types.len())]));
                }
                if next(8) > 0 {
                    violation.insert("speed_over_limit".into(), serde_json::from_str(speeds[next(speeds.len())]).unwrap());
                }
                if next(5) > 0 {
                    violation.insert("in_school_zone".into(), json!(next(2) == 1));
                }
                if next(3) > 0 {
                    violation.insert("location".into(), json!("Main St & 5th Ave"));
                }
                let mut driver = serde_json::Map::new();
                if next(6) > 0 {
                    driver.insert("previous_violations".into(), serde_json::from_str(prevs[next(prevs.len())]).unwrap());
                }
                if next(2) > 0 {
                    driver.insert("license_number".into(), json!(format!("DL{}", next(1000))));
                }
                let mut root = serde_json::Map::new();
                if next(15) > 0 {
                    root.insert("violation".into(), Value::Object(violation));
                }
                if next(15) > 0 {
                    root.insert("driver".into(), Value::Object(driver));
                }
                Value::Object(root)
            })
            .collect()
    }
}

const SEVERITY: [&str; 3] = ["severe", "moderate", "minor"];
const RULE_SEVERITY: [u8; 9] = [0, 1, 2, 1, 2, 0, 1, 0, 2];
const RULE_POINTS: [i64; 9] = [4, 3, 2, 3, 2, 6, 3, 5, 1];
const RULE_FINE: [i64; 9] = [300, 150, 75, 200, 100, 500, 250, 350, 50];
const FINE_FACTOR: [&str; 6] = ["2", "1.5", "1.25", "1.2", "1.1", ""];
const POINTS_FACTOR: [&str; 6] = ["1.5", "", "1.25", "", "", ""];
const RISK: [&str; 3] = ["high", "medium", "low"];
const RISK_RULE: [(u8, i64); 8] = [(0, 15), (1, 15), (1, 30), (1, 45), (2, 15), (2, 30), (2, 45), (2, 45)];
const COMBOS: usize = 9 * 3 * 2;

struct Inputs<'a> {
    kind: (&'a [i32], &'a [u8], &'a [u64]),
    speed: (&'a [Decimal], &'a [u64]),
    school: (&'a [u64], &'a [u64]),
    prev: (&'a [Decimal], &'a [u64]),
}

impl<'a> Inputs<'a> {
    const EMPTY_WORDS: &'static [u64] = &[];

    fn of(built: &'a Built) -> Inputs<'a> {
        let text = |path| match built.at(path) {
            Some((Buf::Text(o, d), v)) => (o.as_slice(), d.as_slice(), v.as_slice()),
            None => (&[][..], &[][..], Self::EMPTY_WORDS),
            _ => panic!("{path} not text"),
        };
        let dec = |path| match built.at(path) {
            Some((Buf::Dec(d), v)) => (d.as_slice(), v.as_slice()),
            None => (&[][..], Self::EMPTY_WORDS),
            _ => panic!("{path} not decimal"),
        };
        let boolean = |path| match built.at(path) {
            Some((Buf::Bool(b), v)) => (b.as_slice(), v.as_slice()),
            None => (Self::EMPTY_WORDS, Self::EMPTY_WORDS),
            _ => panic!("{path} not bool"),
        };
        Inputs {
            kind: text("violation.type"),
            speed: dec("violation.speed_over_limit"),
            school: boolean("violation.in_school_zone"),
            prev: dec("driver.previous_violations"),
        }
    }

    #[inline(always)]
    fn bit(words: &[u64], row: usize) -> bool {
        words.get(row >> 6).is_some_and(|w| w >> (row & 63) & 1 == 1)
    }

    #[inline(always)]
    fn rule1(&self, row: usize, d15: Decimal, d30: Decimal) -> u8 {
        let (offsets, data, valid) = self.kind;
        if !Self::bit(valid, row) {
            return 8;
        }
        let bytes = &data[offsets[row] as usize..offsets[row + 1] as usize];
        match bytes {
            b"speeding" => {
                let (speed, valid) = self.speed;
                match Self::bit(valid, row) {
                    true if speed[row] > d30 => 0,
                    true if speed[row] > d15 => 1,
                    _ => 2,
                }
            }
            b"running_red_light" => 3,
            b"illegal_turn" => 4,
            b"dui" => 5,
            b"driving_without_license" => 6,
            b"reckless_driving" => 7,
            _ => 8,
        }
    }

    #[inline(always)]
    fn above(value: &Decimal, limits: &[i128; 29]) -> bool {
        value.mantissa() > limits[value.scale() as usize]
    }

    fn limits(c: i64) -> [i128; 29] {
        let mut out = [0i128; 29];
        let mut pow = 1i128;
        for slot in out.iter_mut() {
            *slot = c as i128 * pow;
            pow = pow.saturating_mul(10);
        }
        out
    }

    #[inline(always)]
    fn word(bytes: &[u8], at: usize) -> u64 {
        bytes.get(at..at + 8).and_then(|b| b.try_into().ok()).map_or(0, u64::from_le_bytes)
    }

    #[inline(always)]
    fn kind_fast(&self, row: usize) -> u8 {
        let (offsets, data, valid) = self.kind;
        if !Self::bit(valid, row) {
            return 8;
        }
        let (from, to) = (offsets[row] as usize, offsets[row + 1] as usize);
        let bytes = &data[from..to];
        let lit = |b: &[u8], at: usize| Self::word(b, at);
        match bytes.len() {
            8 => match Self::word(bytes, 0) == lit(b"speeding", 0) {
                true => 0,
                false => 8,
            },
            17 if Self::word(bytes, 0) == lit(b"running_red_light", 0) && Self::word(bytes, 8) == lit(b"running_red_light", 8) && bytes[16] == b't' => 3,
            12 if Self::word(bytes, 0) == lit(b"illegal_turn", 0) && Self::word(bytes, 4) == lit(b"illegal_turn", 4) => 4,
            3 if bytes == b"dui" => 5,
            23 if Self::word(bytes, 0) == lit(b"driving_without_license", 0)
                && Self::word(bytes, 8) == lit(b"driving_without_license", 8)
                && Self::word(bytes, 15) == lit(b"driving_without_license", 15) =>
            {
                6
            }
            16 if Self::word(bytes, 0) == lit(b"reckless_driving", 0) && Self::word(bytes, 8) == lit(b"reckless_driving", 8) => 7,
            _ => 8,
        }
    }

    #[inline(always)]
    fn rule1_fast(&self, row: usize, l15: &[i128; 29], l30: &[i128; 29]) -> u8 {
        match self.kind_fast(row) {
            0 => {
                let (speed, valid) = self.speed;
                match Self::bit(valid, row) {
                    true if Self::above(&speed[row], l30) => 0,
                    true if Self::above(&speed[row], l15) => 1,
                    _ => 2,
                }
            }
            other => other,
        }
    }

    #[inline(always)]
    fn prev_fast(&self, row: usize, l2: &[i128; 29], l3: &[i128; 29]) -> u8 {
        let (prev, valid) = self.prev;
        match Self::bit(valid, row) {
            true if Self::above(&prev[row], l3) => 2,
            true if Self::above(&prev[row], l2) => 1,
            _ => 0,
        }
    }

    #[inline(always)]
    fn prev_class(&self, row: usize, d2: Decimal, d3: Decimal) -> u8 {
        let (prev, valid) = self.prev;
        match Self::bit(valid, row) {
            true if prev[row] > d3 => 2,
            true if prev[row] > d2 => 1,
            _ => 0,
        }
    }

    #[inline(always)]
    fn school(&self, row: usize) -> u8 {
        (Self::bit(self.school.1, row) & Self::bit(self.school.0, row)) as u8
    }
}

struct Rules;

impl Rules {
    fn rule2(severity: u8, prev: u8, school: u8) -> u8 {
        match (severity, prev, school) {
            (0, 1 | 2, 1) => 0,
            (0, _, _) => 1,
            (1, 1 | 2, _) => 2,
            (1, _, 1) => 3,
            (2, 2, _) => 4,
            _ => 5,
        }
    }

    fn factor(text: &str) -> Option<Decimal> {
        (!text.is_empty()).then(|| Decimal::from_str(text).unwrap())
    }

    fn finals(r1: usize, r2: usize) -> (Decimal, Decimal) {
        let round = |d: Decimal| d.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
        let fine = Decimal::from(RULE_FINE[r1]);
        let points = Decimal::from(RULE_POINTS[r1]);
        let fine = Self::factor(FINE_FACTOR[r2]).map_or(fine, |f| fine * f);
        let points = Self::factor(POINTS_FACTOR[r2]).map_or(points, |f| points * f);
        (round(fine), round(points))
    }

    fn rule3(points: Decimal, severity: u8) -> u8 {
        let (d3, d5) = (Decimal::from(3), Decimal::from(5));
        match severity {
            0 if points >= d5 => 0,
            0 if points >= d3 => 1,
            1 if points >= d3 => 2,
            2 if points >= d3 => 3,
            0 => 4,
            1 => 5,
            2 => 6,
            _ => 7,
        }
    }
}

struct Entry {
    r1: u8,
    r2: u8,
    r3: u8,
    fine: Decimal,
    points: Decimal,
}

struct Folded {
    entries: Vec<Entry>,
    d2: Decimal,
    d3: Decimal,
    d15: Decimal,
    d30: Decimal,
}

impl Folded {
    fn new() -> Folded {
        let entries = (0..COMBOS)
            .map(|combo| {
                let (r1, prev, school) = (combo / 6, (combo / 2) % 3, combo % 2);
                let r2 = Rules::rule2(RULE_SEVERITY[r1], prev as u8, school as u8);
                let (fine, points) = Rules::finals(r1, r2 as usize);
                Entry {
                    r1: r1 as u8,
                    r2,
                    r3: Rules::rule3(points, RULE_SEVERITY[r1]),
                    fine,
                    points,
                }
            })
            .collect();
        Folded {
            entries,
            d2: Decimal::from(2),
            d3: Decimal::from(3),
            d15: Decimal::from(15),
            d30: Decimal::from(30),
        }
    }

    fn run(&self, inputs: &Inputs, rows: usize, codes: &mut Vec<u8>) {
        codes.clear();
        codes.extend((0..rows).map(|row| {
            let r1 = inputs.rule1(row, self.d15, self.d30);
            let prev = inputs.prev_class(row, self.d2, self.d3);
            r1 * 6 + prev * 2 + inputs.school(row)
        }));
    }

    fn run_fast(&self, inputs: &Inputs, rows: usize, codes: &mut Vec<u8>, limits: &[[i128; 29]; 4]) {
        codes.clear();
        codes.extend((0..rows).map(|row| {
            let r1 = inputs.rule1_fast(row, &limits[1], &limits[2]);
            let prev = inputs.prev_fast(row, &limits[0], &limits[3]);
            r1 * 6 + prev * 2 + inputs.school(row)
        }));
    }

    fn assessment(entry: &Entry) -> Vec<(&'static str, Variable)> {
        let r1 = entry.r1 as usize;
        let (risk, days) = RISK_RULE[entry.r3 as usize];
        vec![
            ("assessment.severity", Variable::String(SEVERITY[RULE_SEVERITY[r1] as usize].into())),
            ("assessment.base_points", Variable::Number(Decimal::from(RULE_POINTS[r1]))),
            ("assessment.base_fine", Variable::Number(Decimal::from(RULE_FINE[r1]))),
            ("assessment.final_fine", Variable::Number(entry.fine)),
            ("assessment.final_points", Variable::Number(entry.points)),
            ("assessment.license_suspension_recommended", Variable::Bool(entry.r2 == 0)),
            ("assessment.risk_level", Variable::String(RISK[risk as usize].into())),
            ("assessment.payment_deadline_days", Variable::Number(Decimal::from(days))),
        ]
    }

    fn row(&self, input: Variable, code: u8) -> Variable {
        for (path, value) in Self::assessment(&self.entries[code as usize]) {
            input.dot_insert(path, value);
        }
        input
    }
}

#[derive(Default)]
struct Staged {
    r1: Vec<u8>,
    r2: Vec<u8>,
    r3: Vec<u8>,
    fine: Vec<Decimal>,
    points: Vec<Decimal>,
}

impl Staged {
    fn run(&mut self, folded: &Folded, inputs: &Inputs, rows: usize) {
        let round = |d: Decimal| d.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero);
        let fine_factor: Vec<Option<Decimal>> = FINE_FACTOR.iter().map(|f| Rules::factor(f)).collect();
        let points_factor: Vec<Option<Decimal>> = POINTS_FACTOR.iter().map(|f| Rules::factor(f)).collect();
        let (d3, d5) = (folded.d3, Decimal::from(5));
        self.r1.clear();
        self.r1.extend((0..rows).map(|row| inputs.rule1(row, folded.d15, folded.d30)));
        self.r2.clear();
        self.r2.extend(self.r1.iter().enumerate().map(|(row, &r1)| {
            Rules::rule2(RULE_SEVERITY[r1 as usize], inputs.prev_class(row, folded.d2, folded.d3), inputs.school(row))
        }));
        self.fine.clear();
        self.points.clear();
        for (&r1, &r2) in self.r1.iter().zip(&self.r2) {
            let fine = Decimal::from(RULE_FINE[r1 as usize]);
            let points = Decimal::from(RULE_POINTS[r1 as usize]);
            self.fine.push(round(fine_factor[r2 as usize].map_or(fine, |f| fine * f)));
            self.points.push(round(points_factor[r2 as usize].map_or(points, |f| points * f)));
        }
        self.r3.clear();
        self.r3.extend(self.r1.iter().zip(&self.points).map(|(&r1, &points)| match RULE_SEVERITY[r1 as usize] {
            0 if points >= d5 => 0,
            0 if points >= d3 => 1,
            1 if points >= d3 => 2,
            2 if points >= d3 => 3,
            0 => 4,
            1 => 5,
            _ => 6,
        }));
    }

    fn row(&self, input: Variable, row: usize) -> Variable {
        let entry = Entry {
            r1: self.r1[row],
            r2: self.r2[row],
            r3: self.r3[row],
            fine: self.fine[row],
            points: self.points[row],
        };
        for (path, value) in Folded::assessment(&entry) {
            input.dot_insert(path, value);
        }
        input
    }
}

#[derive(Default)]
struct Scaled {
    r1: Vec<u8>,
    r2: Vec<u8>,
    r3: Vec<u8>,
    fine: Vec<i64>,
    points: Vec<i64>,
}

impl Scaled {
    const FINE: [(i64, u32); 6] = [(2, 0), (15, 1), (125, 2), (12, 1), (11, 1), (1, 0)];
    const POINTS: [(i64, u32); 6] = [(15, 1), (1, 0), (125, 2), (1, 0), (1, 0), (1, 0)];

    #[inline(always)]
    fn rounded(value: i64, (factor, scale): (i64, u32)) -> i64 {
        let m = value * factor;
        let pow = 10i64.pow(scale);
        let (q, r) = (m / pow, m % pow);
        match 2 * r.abs() >= pow && pow > 1 {
            true => q + m.signum(),
            false => q,
        }
    }

    fn run(&mut self, inputs: &Inputs, rows: usize, limits: &[[i128; 29]; 4]) {
        self.r1.clear();
        self.r1.extend((0..rows).map(|row| inputs.rule1_fast(row, &limits[1], &limits[2])));
        self.r2.clear();
        self.r2.extend(self.r1.iter().enumerate().map(|(row, &r1)| {
            Rules::rule2(RULE_SEVERITY[r1 as usize], inputs.prev_fast(row, &limits[0], &limits[3]), inputs.school(row))
        }));
        self.fine.clear();
        self.points.clear();
        for (&r1, &r2) in self.r1.iter().zip(&self.r2) {
            self.fine.push(Self::rounded(RULE_FINE[r1 as usize], Self::FINE[r2 as usize]));
            self.points.push(Self::rounded(RULE_POINTS[r1 as usize], Self::POINTS[r2 as usize]));
        }
        self.r3.clear();
        self.r3.extend(self.r1.iter().zip(&self.points).map(|(&r1, &points)| match RULE_SEVERITY[r1 as usize] {
            0 if points >= 5 => 0,
            0 if points >= 3 => 1,
            1 if points >= 3 => 2,
            2 if points >= 3 => 3,
            0 => 4,
            1 => 5,
            _ => 6,
        }));
    }

    fn row(&self, input: Variable, row: usize) -> Variable {
        let entry = Entry {
            r1: self.r1[row],
            r2: self.r2[row],
            r3: self.r3[row],
            fine: Decimal::new(self.fine[row], 0),
            points: Decimal::new(self.points[row], 0),
        };
        for (path, value) in Folded::assessment(&entry) {
            input.dot_insert(path, value);
        }
        input
    }
}

struct Ceiling;

impl Ceiling {
    async fn measure(label: &str, inputs_json: &[Value], runs: usize) {
        std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
        let fixture = Fixture::load();
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        assert!(matches!(compiled.compiled_verdict(), Some(Ok(()))));
        let walker = compiled.interpreted();
        let built = Built::new(inputs_json).unwrap();
        let rows = built.rows;
        let columns = built.columns();
        let options = EvaluationOptions::default();
        let inputs = Inputs::of(&built);
        let folded = Folded::new();
        let mut codes = Vec::with_capacity(rows);
        let limits = [Inputs::limits(2), Inputs::limits(15), Inputs::limits(30), Inputs::limits(3)];
        let mut fast = Vec::with_capacity(rows);
        folded.run_fast(&inputs, rows, &mut fast, &limits);
        let mut scaled = Scaled::default();
        scaled.run(&inputs, rows, &limits);
        let mut staged = Staged::default();
        folded.run(&inputs, rows, &mut codes);
        staged.run(&folded, &inputs, rows);
        let output = compiled.evaluate_columns(&columns, options).await;
        let mut mismatches = Vec::new();
        for row in 0..rows {
            let input = columns.row(row);
            let walked = walker.evaluate(input.clone()).await.map(|r| Built::normalized(r.result.to_value())).map_err(|e| e.to_string());
            let engine = match &output.errors[row] {
                Some(e) => Err(e.to_string()),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            let hand = Ok(Built::normalized(folded.row(input.deep_clone(), codes[row]).to_value()));
            let stage = Ok(Built::normalized(staged.row(input.deep_clone(), row).to_value()));
            let scale = Ok(Built::normalized(scaled.row(input.deep_clone(), row).to_value()));
            if walked != hand || walked != engine || walked != stage || walked != scale {
                mismatches.push(format!(
                    "row {row} {}\n walker {walked:?}\n engine {engine:?}\n folded {hand:?}\n staged {stage:?}",
                    input.to_value()
                ));
            }
        }
        assert_eq!(fast, codes);
        assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
        let distinct: std::collections::BTreeSet<u8> = codes.iter().copied().collect();
        let mut best = [f64::MAX; 6];
        let per = |start: std::time::Instant, n: usize| start.elapsed().as_nanos() as f64 / n as f64;
        for _ in 0..runs {
            let start = std::time::Instant::now();
            for c in (0..rows.min(256)).map(|r| columns.row(r)) {
                let _ = walker.evaluate(c).await;
            }
            best[0] = best[0].min(per(start, rows.min(256)));
            let start = std::time::Instant::now();
            let out = compiled.evaluate_columns(&columns, options).await;
            best[1] = best[1].min(per(start, rows));
            drop(std::hint::black_box(out));
            let start = std::time::Instant::now();
            for _ in 0..100 {
                let inputs = Inputs::of(&built);
                folded.run(std::hint::black_box(&inputs), rows, &mut codes);
                std::hint::black_box(&codes);
            }
            best[2] = best[2].min(per(start, rows * 100));
            let start = std::time::Instant::now();
            for _ in 0..100 {
                let inputs = Inputs::of(&built);
                staged.run(&folded, std::hint::black_box(&inputs), rows);
                std::hint::black_box(&staged.fine);
            }
            best[3] = best[3].min(per(start, rows * 100));
            let start = std::time::Instant::now();
            for _ in 0..100 {
                let inputs = Inputs::of(&built);
                folded.run_fast(std::hint::black_box(&inputs), rows, &mut fast, &limits);
                std::hint::black_box(&fast);
            }
            best[4] = best[4].min(per(start, rows * 100));
            let start = std::time::Instant::now();
            for _ in 0..100 {
                let inputs = Inputs::of(&built);
                scaled.run(std::hint::black_box(&inputs), rows, &limits);
                std::hint::black_box(&scaled.fine);
            }
            best[5] = best[5].min(per(start, rows * 100));
        }
        println!(
            "{label}: rows {rows} distinct combos {} | walker {:.0} ns/row | engine {:.1} ns/row | hand folded {:.2} ns/row | hand folded-fast {:.2} ns/row | hand staged-decimal {:.2} ns/row | hand staged-scaled {:.2} ns/row | engine/folded {:.1}x engine/staged {:.1}x | walker/engine {:.1}x walker/folded {:.0}x walker/staged {:.0}x",
            distinct.len(),
            best[0],
            best[1],
            best[2],
            best[4],
            best[3],
            best[5],
            best[1] / best[2],
            best[1] / best[3],
            best[0] / best[1],
            best[0] / best[2],
            best[0] / best[3],
        );
    }
}

#[tokio::test]
#[ignore]
async fn traffic_ceiling() {
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let runs: usize = std::env::var("BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(7);
    let fixture = Fixture::load();
    let variants = fixture.typed_variants();
    let bench: Vec<Value> = (0..rows).map(|i| variants[i % variants.len()].clone()).collect();
    Ceiling::measure("bench-input", &bench, runs).await;
    Ceiling::measure("random-input", &Fixture::random(rows), runs).await;
}

#[tokio::test]
#[ignore]
async fn traffic_profile_engine() {
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let fixture = Fixture::load();
    let variants = fixture.typed_variants();
    let random = std::env::var("BENCH_RANDOM").is_ok();
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let inputs: Vec<Value> = match random {
        true => Fixture::random(rows),
        false => (0..rows).map(|i| variants[i % variants.len()].clone()).collect(),
    };
    let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
    compiled.compile();
    let built = Built::new(&inputs).unwrap();
    let columns = built.columns();
    let seconds: u64 = std::env::var("BENCH_PROFILE").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut calls = 0usize;
    while std::time::Instant::now() < until {
        let _ = compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
        calls += 1;
    }
    println!("calls {calls}");
}

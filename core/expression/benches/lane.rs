use rust_decimal::Decimal;
use serde_json::Value;
use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::{Duration, Instant};
use zen_expression::lane::{Column, Columns, LaneProgram, LaneRunner, Values};
use zen_expression::variable::VariableType;
use zen_expression::vm::VM;
use zen_expression::{ExpressionKind, Isolate, Scope, Variable};

const ROWS: usize = 512;
const GRAPHS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/graphs");

struct Case {
    source: String,
    scopes: Vec<Scope>,
}

struct Bench;

enum Data {
    I64(Vec<i64>),
    Dec(Vec<Decimal>),
    Bool(Vec<u64>),
    Str(Vec<String>),
    Dict(Vec<i32>, Vec<String>),
    ListDec(Vec<i32>, Vec<Decimal>),
    ListI64(Vec<i32>, Vec<i64>),
    ListStr(Vec<i32>, Vec<String>),
    ListStruct(Vec<i32>, Vec<(String, Field, Vec<u64>)>, usize),
    Any(Vec<Variable>),
}

enum Field {
    Scaled(Vec<i64>, Vec<u8>),
    Utf8(Vec<i32>, Vec<u8>),
    Bool(Vec<u64>),
    Any(Vec<Variable>),
}

impl Field {
    fn of(cells: &[Option<Variable>]) -> (Field, Vec<u64>) {
        let mut valid = vec![0u64; cells.len().div_ceil(64).max(1)];
        cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.as_ref().is_some_and(|v| !matches!(v, Variable::Null)))
            .for_each(|(i, _)| valid[i / 64] |= 1 << (i % 64));
        let present: Vec<&Variable> = cells.iter().flatten().filter(|v| !matches!(v, Variable::Null)).collect();
        let field = match present.first() {
            Some(Variable::Number(_)) if present.iter().all(|v| matches!(v, Variable::Number(n) if i64::try_from(n.mantissa()).is_ok())) => {
                let parts: Vec<(i64, u8)> = cells
                    .iter()
                    .map(|c| match c {
                        Some(Variable::Number(n)) => (i64::try_from(n.mantissa()).unwrap_or(0), n.scale() as u8),
                        _ => (0, 0),
                    })
                    .collect();
                Field::Scaled(parts.iter().map(|p| p.0).collect(), parts.iter().map(|p| p.1).collect())
            }
            Some(Variable::String(_)) if present.iter().all(|v| matches!(v, Variable::String(_))) => {
                let mut offsets = vec![0i32];
                let mut data = Vec::new();
                for c in cells {
                    if let Some(Variable::String(t)) = c {
                        data.extend_from_slice(t.as_bytes());
                    }
                    offsets.push(data.len() as i32);
                }
                Field::Utf8(offsets, data)
            }
            Some(Variable::Bool(_)) if present.iter().all(|v| matches!(v, Variable::Bool(_))) => {
                let mut bits = vec![0u64; cells.len().div_ceil(64).max(1)];
                cells
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| matches!(c, Some(Variable::Bool(true))))
                    .for_each(|(i, _)| bits[i / 64] |= 1 << (i % 64));
                Field::Bool(bits)
            }
            _ => Field::Any(cells.iter().map(|c| c.clone().unwrap_or(Variable::Null)).collect()),
        };
        (field, valid)
    }

    fn values(&self) -> Values<'_> {
        match self {
            Field::Scaled(mant, scale) => Values::Scaled { mant, scale },
            Field::Utf8(offsets, data) => Values::Utf8 { offsets, data },
            Field::Bool(bits) => Values::Bool { bits, offset: 0 },
            Field::Any(values) => Values::Any(values),
        }
    }
}

struct Table {
    rows: usize,
    keys: Vec<String>,
    data: Vec<Data>,
    valid: Vec<Vec<u64>>,
}

impl Table {
    fn flatten(value: &Variable, prefix: String, out: &mut BTreeMap<String, Variable>) {
        match value {
            Variable::Object(map) if !map.borrow().is_empty() => {
                for (k, v) in map.borrow().iter() {
                    let path = if prefix.is_empty() {
                        k.to_string()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    Self::flatten(v, path, out);
                }
            }
            v if !prefix.is_empty() => {
                out.insert(prefix, v.clone());
            }
            _ => {}
        }
    }

    fn build(scopes: &[Scope]) -> Self {
        let flat: Vec<BTreeMap<String, Variable>> = scopes
            .iter()
            .map(|s| {
                let mut m = BTreeMap::new();
                Self::flatten(s.base(), String::new(), &mut m);
                m
            })
            .collect();
        let mut keys: Vec<String> = flat.iter().flat_map(|m| m.keys().cloned()).collect();
        keys.sort();
        keys.dedup();
        let rows = scopes.len();
        let mut data = Vec::new();
        let mut valid = Vec::new();
        for key in &keys {
            let values: Vec<Option<&Variable>> = flat.iter().map(|m| m.get(key)).collect();
            let mut bits = vec![0u64; rows.div_ceil(64)];
            for (i, v) in values.iter().enumerate() {
                if v.is_some_and(|v| !matches!(v, Variable::Null)) {
                    bits[i / 64] |= 1 << (i % 64);
                }
            }
            let present: Vec<&Variable> = values
                .iter()
                .flatten()
                .copied()
                .filter(|v| !matches!(v, Variable::Null))
                .collect();
            let all =
                |f: fn(&Variable) -> bool| !present.is_empty() && present.iter().all(|v| f(v));
            let integral = |v: &Variable| matches!(v, Variable::Number(n) if n.scale() == 0 && i64::try_from(n.mantissa()).is_ok());
            let column = if !present.is_empty() && present.iter().all(|v| integral(v)) {
                Data::I64(
                    values
                        .iter()
                        .map(|v| match v {
                            Some(Variable::Number(n)) => i64::try_from(n.mantissa()).unwrap_or(0),
                            _ => 0,
                        })
                        .collect(),
                )
            } else if all(|v| matches!(v, Variable::Number(_))) {
                Data::Dec(
                    values
                        .iter()
                        .map(|v| match v {
                            Some(Variable::Number(n)) => *n,
                            _ => Decimal::ZERO,
                        })
                        .collect(),
                )
            } else if all(|v| matches!(v, Variable::Bool(_))) {
                let mut words = vec![0u64; rows.div_ceil(64)];
                for (i, v) in values.iter().enumerate() {
                    if matches!(v, Some(Variable::Bool(true))) {
                        words[i / 64] |= 1 << (i % 64);
                    }
                }
                Data::Bool(words)
            } else if all(|v| matches!(v, Variable::String(_)))
                && std::env::var("LANE_DICT").is_ok()
            {
                let mut distinct: Vec<String> = Vec::new();
                let codes = values
                    .iter()
                    .map(|v| {
                        let text = v.and_then(|v| v.as_str()).unwrap_or_default();
                        match distinct.iter().position(|d| d == text) {
                            Some(i) => i as i32,
                            None => {
                                distinct.push(text.to_string());
                                (distinct.len() - 1) as i32
                            }
                        }
                    })
                    .collect();
                Data::Dict(codes, distinct)
            } else if all(|v| matches!(v, Variable::String(_))) {
                Data::Str(
                    values
                        .iter()
                        .map(|v| v.and_then(|v| v.as_str()).unwrap_or_default().to_string())
                        .collect(),
                )
            } else if std::env::var("LANE_ANY_LISTS").is_err()
                && all(
                    |v| matches!(v, Variable::Array(a) if a.borrow().iter().all(|x| matches!(x, Variable::Number(_)))),
                )
            {
                let mut offsets = vec![0i32];
                let mut items = Vec::new();
                for v in &values {
                    if let Some(Variable::Array(a)) = v {
                        items.extend(a.borrow().iter().map(|x| match x {
                            Variable::Number(n) => *n,
                            _ => Decimal::ZERO,
                        }));
                    }
                    offsets.push(items.len() as i32);
                }
                match items
                    .iter()
                    .all(|n| n.scale() == 0 && i64::try_from(n.mantissa()).is_ok())
                {
                    true => Data::ListI64(
                        offsets,
                        items
                            .iter()
                            .map(|n| i64::try_from(n.mantissa()).unwrap_or(0))
                            .collect(),
                    ),
                    false => Data::ListDec(offsets, items),
                }
            } else if std::env::var("LANE_ANY_LISTS").is_err()
                && all(
                    |v| matches!(v, Variable::Array(a) if a.borrow().iter().all(|x| matches!(x, Variable::String(_)))),
                )
            {
                let mut offsets = vec![0i32];
                let mut items = Vec::new();
                for v in &values {
                    if let Some(Variable::Array(a)) = v {
                        items.extend(
                            a.borrow()
                                .iter()
                                .map(|x| x.as_str().unwrap_or_default().to_string()),
                        );
                    }
                    offsets.push(items.len() as i32);
                }
                Data::ListStr(offsets, items)
            } else if std::env::var("LANE_ANY_LISTS").is_err()
                && all(
                    |v| matches!(v, Variable::Array(a) if a.borrow().iter().all(|x| matches!(x, Variable::Object(_)))),
                )
            {
                let mut offsets = vec![0i32];
                let mut items: Vec<Variable> = Vec::new();
                for v in &values {
                    if let Some(Variable::Array(a)) = v {
                        items.extend(a.borrow().iter().cloned());
                    }
                    offsets.push(items.len() as i32);
                }
                let mut names: Vec<String> = Vec::new();
                for item in &items {
                    if let Variable::Object(o) = item {
                        for (k, _) in o.borrow().iter() {
                            if !names.iter().any(|n| n.as_str() == k.as_ref() as &str) {
                                names.push(k.to_string());
                            }
                        }
                    }
                }
                let fields = names
                    .into_iter()
                    .map(|name| {
                        let cells: Vec<Option<Variable>> = items
                            .iter()
                            .map(|item| match item {
                                Variable::Object(o) => o.borrow().get_str(&name).cloned(),
                                _ => None,
                            })
                            .collect();
                        let (field, valid) = Field::of(&cells);
                        (name, field, valid)
                    })
                    .collect();
                Data::ListStruct(offsets, fields, items.len())
            } else {
                Data::Any(
                    values
                        .iter()
                        .map(|v| v.cloned().unwrap_or(Variable::Null))
                        .collect(),
                )
            };
            data.push(column);
            valid.push(bits);
        }
        Self {
            rows,
            keys,
            data,
            valid,
        }
    }

    fn fields(&self) -> Vec<Vec<(&str, Column<'_>)>> {
        self.data
            .iter()
            .map(|d| match d {
                Data::ListStruct(_, fields, _) => fields
                    .iter()
                    .map(|(name, field, valid)| (name.as_str(), Column::with_validity(field.values(), valid, 0)))
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    fn children<'a>(&'a self, texts: &'a [(Vec<i32>, Vec<u8>)], fields: &'a [Vec<(&'a str, Column<'a>)>]) -> Vec<Column<'a>> {
        self.data
            .iter()
            .enumerate()
            .map(|(i, d)| match d {
                Data::ListStruct(_, _, len) => Column::new(Values::Struct { fields: &fields[i], len: *len }),
                Data::ListDec(_, items) => Column::new(Values::Dec(items)),
                Data::ListI64(_, items) => Column::new(Values::I64(items)),
                Data::ListStr(..) | Data::Dict(..) => Column::new(Values::Utf8 {
                    offsets: &texts[i].0,
                    data: &texts[i].1,
                }),
                _ => Column::new(Values::Any(&[])),
            })
            .collect()
    }

    fn columns<'a>(
        &'a self,
        texts: &'a [(Vec<i32>, Vec<u8>)],
        children: &'a [Column<'a>],
    ) -> Columns<'a> {
        let mut columns = Columns::new(self.rows);
        for (i, ((key, data), valid)) in self
            .keys
            .iter()
            .zip(&self.data)
            .zip(&self.valid)
            .enumerate()
        {
            let values = match data {
                Data::I64(v) => Values::I64(v),
                Data::Dec(v) => Values::Dec(v),
                Data::Bool(bits) => Values::Bool { bits, offset: 0 },
                Data::Str(_) => Values::Utf8 {
                    offsets: &texts[i].0,
                    data: &texts[i].1,
                },
                Data::ListDec(offsets, _)
                | Data::ListI64(offsets, _)
                | Data::ListStr(offsets, _)
                | Data::ListStruct(offsets, _, _) => Values::List {
                    offsets,
                    child: (&children[i]).into(),
                },
                Data::Dict(keys, _) => Values::Dict {
                    keys,
                    values: (&children[i]).into(),
                },
                Data::Any(v) => Values::Any(v),
            };
            columns = columns.column(key, Column::with_validity(values, valid, 0));
        }
        columns
    }

    fn texts(&self) -> Vec<(Vec<i32>, Vec<u8>)> {
        self.data
            .iter()
            .map(|d| match d {
                Data::Str(v) | Data::ListStr(_, v) | Data::Dict(_, v) => {
                    let mut offsets = vec![0i32];
                    let mut data = Vec::new();
                    for text in v {
                        data.extend_from_slice(text.as_bytes());
                        offsets.push(data.len() as i32);
                    }
                    (offsets, data)
                }
                _ => (Vec::new(), Vec::new()),
            })
            .collect()
    }
}

impl Bench {
    fn cases() -> Vec<Case> {
        let mut names: Vec<_> = std::fs::read_dir(GRAPHS)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        names.sort();
        let mut out = Vec::new();
        for path in names {
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(doc) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let inputs: Vec<Variable> = doc["tests"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.get("input").cloned())
                .map(Variable::from)
                .collect();
            if inputs.is_empty() {
                continue;
            }
            for node in doc["nodes"].as_array().into_iter().flatten() {
                if node["type"] != "expressionNode" {
                    continue;
                }
                for e in node["content"]["expressions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let Some(source) = e["value"].as_str() else {
                        continue;
                    };
                    let scopes: Vec<Scope> = (0..ROWS)
                        .map(|i| Scope::new(inputs[i % inputs.len()].depth_clone(64)))
                        .collect();
                    let ok = scopes.iter().take(inputs.len()).all(|s| {
                        let mut iso = Isolate::new();
                        iso.set_environment(s.base().clone());
                        matches!(iso.run_standard(source), Ok(v) if v != Variable::Null)
                    });
                    if ok && !source.contains("date(") {
                        out.push(Case {
                            source: source.to_string(),
                            scopes,
                        });
                    }
                }
            }
        }
        out
    }

    fn timed(budget: Duration, mut f: impl FnMut()) -> f64 {
        f();
        let start = Instant::now();
        let mut n = 0u64;
        while start.elapsed() < budget {
            f();
            n += 1;
        }
        start.elapsed().as_nanos() as f64 / n as f64
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    }

    fn geomean(v: &[f64]) -> f64 {
        (v.iter().map(|x| x.ln()).sum::<f64>() / v.len() as f64).exp()
    }
}

fn micro() -> bool {
    let Ok(list) = std::env::var("LANE_EXPR") else {
        return false;
    };
    let input = Variable::from(serde_json::json!({
        "a": 5, "b": 7, "s": "gold", "flag": true, "items": [1, 2, 3, 4, 5, 6, 7, 8],
        "tags": ["x", "diabetes", "y"], "o": {"x": 1, "y": {"z": 2}}, "d1": "2025-03-15T10:30:00Z", "d2": "2025-03-15", "d3": "2025-03-15 10:30:00",
        "objs": [{"x": 1, "k": "a"}, {"x": 5, "k": "b"}, {"x": 9, "k": "c"}, {"x": 2, "k": "a"}],
        "email": "john.doe@example.com", "phone": "+1-555-0100"
    }));
    let rows: usize = std::env::var("LANE_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(ROWS);
    let mut runner = LaneRunner::new();
    let budget = Duration::from_millis(300);
    for entry in list.split('|') {
        let (reference, source) = match entry.split_once(':') {
            Some(("u", rest)) => (Some(Variable::Number(5.into())), rest),
            Some(("us", rest)) => (Some(Variable::String("gold".into())), rest),
            _ => (None, entry),
        };
        let vary = std::env::var("LANE_VARY").is_ok();
        let scopes: Vec<Scope> = (0..rows)
            .map(|i| {
                let base = match vary {
                    true => {
                        let mut value = input.depth_clone(64);
                        if let Variable::Object(o) = &mut value {
                            let mut o = o.borrow_mut();
                            let day = |k: usize| {
                                format!("20{:02}-{:02}-{:02}", 10 + k % 15, 1 + k % 12, 1 + k % 28)
                            };
                            o.insert(
                                "a".into(),
                                Variable::Number(Decimal::from(((i * 7919) % 97) as i64 - 40)),
                            );
                            o.insert(
                                "b".into(),
                                Variable::Number(Decimal::from(((i * 104729) % 89) as i64 - 30)),
                            );
                            o.insert(
                                "d1".into(),
                                Variable::String(
                                    format!("{}T{:02}:30:00Z", day(i), i % 24).as_str().into(),
                                ),
                            );
                            o.insert(
                                "d2".into(),
                                Variable::String(day(i * 31 + 7).as_str().into()),
                            );
                            o.insert(
                                "s".into(),
                                Variable::String(
                                    ["gold", "silver", "Bronze", "platinum"][i % 4].into(),
                                ),
                            );
                        }
                        value
                    }
                    false => input.depth_clone(64),
                };
                let mut scope = Scope::new(base);
                if let Some(r) = &reference {
                    scope.set_local(Variable::dollar_key(), r.clone());
                }
                scope
            })
            .collect();
        let program = match reference {
            Some(_) => LaneProgram::unary(source),
            None => LaneProgram::standard(source),
        }
        .expect("lane compile")
        .specialize(&scopes)
        .expect("specialize");
        let mut vm = VM::new();
        let s = match reference {
            Some(_) => {
                let expression = Isolate::new().compile_unary(source).expect("compile");
                Bench::timed(budget, || {
                    for scope in &scopes {
                        black_box(expression.evaluate_with_scope(scope, &mut vm).is_ok());
                    }
                })
            }
            None => {
                let expression = Isolate::new().compile_standard(source).expect("compile");
                Bench::timed(budget, || {
                    for scope in &scopes {
                        black_box(expression.evaluate_with_scope(scope, &mut vm).is_ok());
                    }
                })
            }
        } / rows as f64;
        if std::env::var("LANE_DUMP").is_ok() {
            for step in &program.program().steps {
                println!("  {:?}", step);
            }
        }
        let profile = std::env::var("LANE_PROFILE").is_ok();
        let one = Bench::timed(
            if profile {
                Duration::from_secs(4)
            } else {
                budget
            },
            || {
                for scope in &scopes {
                    black_box(runner.evaluate_one(&program, scope).is_ok());
                }
            },
        ) / rows as f64;
        if profile {
            println!("{source} lane-w1 {one:.1}");
            continue;
        }
        let batch = Bench::timed(budget, || {
            let mut n = 0usize;
            runner.evaluate_with(&program, &scopes, |_, r| n += r.is_ok() as usize);
            black_box(n);
        }) / rows as f64;
        let table = Table::build(&scopes);
        let texts = table.texts();
        let fields = table.fields();
        let children = table.children(&texts, &fields);
        let columns = table.columns(&texts, &children);
        let columnar = match reference {
            Some(_) => None,
            None => LaneProgram::standard(source)
                .ok()
                .and_then(|p| p.specialize_columns(&columns).ok()),
        };
        let mut out = zen_expression::lane::Output::new();
        if let (Ok(_), Some(p)) = (std::env::var("LANE_PROFILE_COLS"), &columnar) {
            let ns = Bench::timed(Duration::from_secs(4), || {
                runner.evaluate_columns_into(p, &columns, &mut out);
                black_box(out.len());
            }) / rows as f64;
            println!("{entry} cols {ns:.1}");
            continue;
        }
        let cols = match &columnar {
            Some(p) => {
                Bench::timed(budget, || {
                    runner.evaluate_columns_into(p, &columns, &mut out);
                    black_box(out.len());
                }) / rows as f64
            }
            None => f64::NAN,
        };
        if std::env::var("LANE_TABLE").is_ok() {
            let generic = match reference {
                Some(_) => LaneProgram::unary(source),
                None => LaneProgram::standard(source),
            }
            .expect("lane compile");
            let cold1 = Bench::timed(budget, || {
                for scope in &scopes {
                    black_box(runner.evaluate_one(&generic, scope).is_ok());
                }
            }) / rows as f64;
            let cold64 = Bench::timed(budget, || {
                let mut n = 0usize;
                runner.evaluate_with(&generic, &scopes, |_, r| n += r.is_ok() as usize);
                black_box(n);
            }) / rows as f64;
            println!(
                "TSV\t{entry}\t{s:.1}\t{cold1:.1}\t{cold64:.1}\t{one:.1}\t{batch:.1}\t{cols:.1}"
            );
            continue;
        }
        println!(
            "{:50} ops {:3} stack {s:7.1} lane-w1 {one:7.1} lane-w64 {batch:7.1} cols {cols:6.1} | w1 {:5.2}x w64 {:5.2}x cols {:5.1}x",
            entry,
            program.program().steps.len(),
            s / one,
            s / batch,
            s / cols
        );
    }
    true
}

fn cells() -> bool {
    if std::env::var("LANE_CELLS").is_err() {
        return false;
    }
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    let mut next = |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    for rules in [100usize, 1000, 8000] {
        let cells: Vec<String> = (0..rules)
            .map(|r| match (next(10), r + 1 == rules) {
                (_, true) | (0..=2, _) => String::new(),
                (3 | 4, _) => format!(
                    "'{}'",
                    ["US", "GB", "DE", "FR", "ES", "IT", "NL", "PL"][next(8) as usize]
                ),
                (5 | 6, _) => format!(
                    "{} {}",
                    ["<", "<=", ">", ">="][next(4) as usize],
                    next(20) * 5
                ),
                (7, _) => format!("[{}..{}]", next(10) * 5, 50 + next(10) * 5),
                (8, _) => format!(
                    "'{}', '{}'",
                    ["US", "GB"][next(2) as usize],
                    ["DE", "FR"][next(2) as usize]
                ),
                _ => "$ > limit".to_string(),
            })
            .collect();
        let refs: Vec<Option<&str>> = cells.iter().map(|c| Some(c.as_str())).collect();
        let set = zen_expression::lane::CellSet::compile(&refs).expect("cells");
        let values: Vec<Variable> = (0..ROWS)
            .map(|i| match i % 2 {
                0 => Variable::from(serde_json::json!(
                    ["US", "GB", "DE", "SE"][next(4) as usize]
                )),
                _ => Variable::from(serde_json::json!(next(100))),
            })
            .collect();
        let envs: Vec<Scope> = (0..ROWS)
            .map(|_| Scope::new(serde_json::json!({"limit": next(50)}).into()))
            .collect();
        let exprs: Vec<Option<zen_expression::Expression<zen_expression::expression::Unary>>> =
            cells
                .iter()
                .map(|c| (!c.is_empty()).then(|| Isolate::new().compile_unary(c).expect("unary")))
                .collect();
        let mut vm = VM::new();
        let stack = Bench::timed(Duration::from_millis(400), || {
            let mut hits = 0usize;
            for (value, env) in values.iter().zip(&envs).take(64) {
                let mut scope = Scope::new(env.base().clone());
                scope.set_local(Variable::dollar_key(), value.clone());
                for e in &exprs {
                    hits += match e {
                        None => 1,
                        Some(e) => e.evaluate_with_scope(&scope, &mut vm).unwrap_or(false) as usize,
                    };
                }
            }
            black_box(hits);
        }) / 64.0;
        let mut runner = LaneRunner::new();
        let mut out = Vec::new();
        let lane = Bench::timed(Duration::from_millis(400), || {
            set.evaluate(&mut runner, &values, &envs, &mut out);
            black_box(out.len());
        }) / ROWS as f64;
        println!("cells rules {rules:5}: evaluate every cell (stack VM) {stack:10.0} ns/row | cell set {lane:8.1} ns/row | {:6.1}x", stack / lane);
    }
    true
}

fn nodes() -> bool {
    if std::env::var("LANE_NODES").is_err() {
        return false;
    }
    let mut names: Vec<_> = std::fs::read_dir(GRAPHS)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    names.sort();
    let (mut stack_all, mut one_all, mut batch_all) = (Vec::new(), Vec::new(), Vec::new());
    let mut runner = LaneRunner::new();
    for path in names {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let inputs: Vec<Variable> = doc["tests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.get("input").cloned())
            .map(Variable::from)
            .collect();
        if inputs.is_empty() {
            continue;
        }
        for node in doc["nodes"].as_array().into_iter().flatten() {
            if node["type"] != "expressionNode"
                || node["content"]["inputField"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            {
                continue;
            }
            let entries: Vec<(String, String)> = node["content"]["expressions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    Some((
                        e["key"].as_str()?.to_string(),
                        e["value"].as_str()?.to_string(),
                    ))
                })
                .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                .collect();
            if entries.is_empty() {
                continue;
            }
            let refs: Vec<(&str, &str)> = entries
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let Ok(program) = LaneProgram::compile_many(&refs, true) else {
                continue;
            };
            let scopes: Vec<Scope> = (0..ROWS)
                .map(|i| Scope::new(inputs[i % inputs.len()].depth_clone(64)))
                .collect();
            let program = program.specialize(&scopes).expect("specialize");
            let codes: Vec<(
                String,
                zen_expression::Expression<zen_expression::expression::Standard>,
            )> = entries
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), Isolate::new().compile_standard(v).ok()?)))
                .collect();
            if codes.len() != entries.len() {
                continue;
            }
            let mut vm = VM::new();
            let budget = Duration::from_millis(80);
            let stack = Bench::timed(budget, || {
                for scope in &scopes {
                    let mut scope = scope.clone();
                    let dollar = Variable::empty_object();
                    scope.set_local(Variable::dollar_key(), dollar.clone());
                    for (key, e) in &codes {
                        match e.evaluate_with_scope(&scope, &mut vm) {
                            Ok(v) => {
                                dollar.dot_insert(key, v);
                            }
                            Err(_) => break,
                        }
                    }
                    black_box(&dollar);
                }
            }) / ROWS as f64;
            let one = Bench::timed(budget, || {
                for scope in &scopes {
                    runner.evaluate_many(&program, std::slice::from_ref(scope), None, |_, r| {
                        black_box(r.is_some());
                    });
                }
            }) / ROWS as f64;
            let batch = Bench::timed(budget, || {
                runner.evaluate_many(&program, &scopes, None, |_, r| {
                    black_box(r.is_some());
                });
            }) / ROWS as f64;
            stack_all.push(stack);
            one_all.push(one);
            batch_all.push(batch);
        }
    }
    let r1: Vec<f64> = stack_all.iter().zip(&one_all).map(|(s, l)| s / l).collect();
    let r64: Vec<f64> = stack_all
        .iter()
        .zip(&batch_all)
        .map(|(s, l)| s / l)
        .collect();
    println!(
        "expression nodes {} | median ns/row: stack {:.0}, lane w1 {:.0}, lane w64 {:.0} | stack/lane geomean: w1 {:.2}x, w64 {:.2}x | sums: stack {:.0}, w1 {:.0}, w64 {:.0}",
        stack_all.len(),
        Bench::median(stack_all.clone()),
        Bench::median(one_all.clone()),
        Bench::median(batch_all.clone()),
        Bench::geomean(&r1),
        Bench::geomean(&r64),
        stack_all.iter().sum::<f64>(),
        one_all.iter().sum::<f64>(),
        batch_all.iter().sum::<f64>()
    );
    true
}

fn main() {
    if scenarios() {
        return;
    }
    if nodes() {
        return;
    }
    if cells() {
        return;
    }
    if micro() {
        return;
    }
    let filter = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
    let budget = Duration::from_millis(60);
    let cases = Bench::cases();
    let (mut stack, mut w1, mut w64, mut cols, mut outs) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut typing: Vec<[f64; 9]> = Vec::new();
    let mut runner = LaneRunner::new();
    for case in &cases {
        if let Some(f) = &filter {
            if !case.source.contains(f.as_str()) {
                continue;
            }
        }
        let expression = Isolate::new()
            .compile_standard(&case.source)
            .expect("compile");
        let generic = LaneProgram::standard(&case.source).expect("lane compile");
        let program = generic.specialize(&case.scopes).expect("specialize");
        let mut vm = VM::new();
        let only_columns = std::env::var("LANE_ONLY_COLUMNS").is_ok();
        let budget = if only_columns {
            Duration::from_millis(1)
        } else {
            budget
        };
        let s = Bench::timed(budget, || {
            for scope in &case.scopes {
                black_box(expression.evaluate_with_scope(scope, &mut vm).is_ok());
            }
        }) / ROWS as f64;
        let one = Bench::timed(budget, || {
            for scope in &case.scopes {
                black_box(runner.evaluate_one(&program, scope).is_ok());
            }
        }) / ROWS as f64;
        let batch = Bench::timed(budget, || {
            let mut n = 0usize;
            runner.evaluate_with(&program, &case.scopes, |_, r| n += r.is_ok() as usize);
            black_box(n);
        }) / ROWS as f64;
        let table = Table::build(&case.scopes);
        let texts = table.texts();
        let fields = table.fields();
        let children = table.children(&texts, &fields);
        let columns = table.columns(&texts, &children);
        let columnar = generic.specialize_columns(&columns).expect("specialize");
        let col = Bench::timed(
            if only_columns {
                Duration::from_millis(300)
            } else {
                budget
            },
            || {
                let mut n = 0usize;
                runner.evaluate_columns(&columnar, &columns, |_, r| n += r.is_ok() as usize);
                black_box(n);
            },
        ) / ROWS as f64;
        let mut out = zen_expression::lane::Output::new();
        let typed = Bench::timed(
            if only_columns {
                Duration::from_millis(300)
            } else {
                budget
            },
            || {
                runner.evaluate_columns_into(&columnar, &columns, &mut out);
                black_box(out.len());
            },
        ) / ROWS as f64;
        if std::env::var("LANE_DEBUG").is_ok() {
            let p = columnar.program();
            let bound: Vec<zen_expression::lane::Binding> = p
                .site_keys
                .iter()
                .map(|k| {
                    k.as_deref()
                        .map_or(zen_expression::lane::Binding::Row, |k| columns.bind(k))
                })
                .collect();
            println!(
                "keys {:?} bound {:?} rows {} kinds {:?}",
                p.site_keys,
                bound,
                p.needs_rows(&bound),
                p.kinds
            );
            for (k, c) in &columns.columns {
                if k.contains("status") || k.contains("watch") {
                    println!("  column {k} {:?}", c.kind());
                }
            }
        }
        if std::env::var("LANE_DUMP").is_ok() {
            println!("input {:?}", case.scopes[0].base().to_value());
            for step in &program.program().steps {
                println!("  {:?}", step);
            }
        }
        if filter.is_some() {
            println!("{:70} stack {s:8.1} lane-w1 {one:8.1} lane-w64 {batch:8.1} columns {col:8.1} typed {typed:8.1}", &case.source[..case.source.len().min(70)]);
        }
        stack.push(s);
        w1.push(one);
        w64.push(batch);
        cols.push(col);
        outs.push(typed);
        if std::env::var("LANE_TYPING").is_ok() {
            let first = VariableType::from(&case.scopes[0].base().to_value());
            let typed_program =
                LaneProgram::compile_typed(&case.source, ExpressionKind::Standard, &first)
                    .expect("typed");
            let leg = |p: &LaneProgram, runner: &mut LaneRunner| {
                let w1 = Bench::timed(budget, || {
                    for scope in &case.scopes {
                        black_box(runner.evaluate_one(p, scope).is_ok());
                    }
                }) / ROWS as f64;
                let w64 = Bench::timed(budget, || {
                    let mut n = 0usize;
                    runner.evaluate_with(p, &case.scopes, |_, r| n += r.is_ok() as usize);
                    black_box(n);
                }) / ROWS as f64;
                (w1, w64)
            };
            let (g1, g64) = leg(&generic, &mut runner);
            let (t1, t64) = leg(&typed_program, &mut runner);
            let gc = Bench::timed(budget, || {
                runner.evaluate_columns_into(&generic, &columns, &mut out);
                black_box(out.len());
            }) / ROWS as f64;
            typing.push([s, one, batch, typed, g1, g64, gc, t1, t64]);
        }
    }
    let r1: Vec<f64> = stack.iter().zip(&w1).map(|(s, l)| s / l).collect();
    let r64: Vec<f64> = stack.iter().zip(&w64).map(|(s, l)| s / l).collect();
    let rc: Vec<f64> = stack.iter().zip(&cols).map(|(s, l)| s / l).collect();
    let ro: Vec<f64> = stack.iter().zip(&outs).map(|(s, l)| s / l).collect();
    if !typing.is_empty() {
        let labels = [
            "speculated w1",
            "speculated w64",
            "column-typed output",
            "generic w1",
            "generic w64",
            "generic columns output",
            "compile_typed w1",
            "compile_typed w64",
        ];
        for (i, label) in labels.iter().enumerate() {
            let ratios: Vec<f64> = typing.iter().map(|t| t[0] / t[i + 1]).collect();
            let times: Vec<f64> = typing.iter().map(|t| t[i + 1]).collect();
            println!(
                "typing | {label:24} geomean {:5.2}x median {:5.2}x | median {:6.1} ns/row",
                Bench::geomean(&ratios),
                Bench::median(ratios.clone()),
                Bench::median(times)
            );
        }
    }
    println!(
        "typed output: median {:.1} ns/row, stack/typed geomean {:.2}x, median {:.2}x, sum {:.0}",
        Bench::median(outs.clone()),
        Bench::geomean(&ro),
        Bench::median(ro.clone()),
        outs.iter().sum::<f64>()
    );
    println!(
        "columns: median {:.1} ns/row, stack/columns geomean {:.2}x, median {:.2}x, sum {:.0}",
        Bench::median(cols.clone()),
        Bench::geomean(&rc),
        Bench::median(rc.clone()),
        cols.iter().sum::<f64>()
    );
    println!(
        "{} expressions | median ns/eval: stack {:.1}, lane w1 {:.1}, lane w64 {:.1} | stack/lane geomean: w1 {:.2}x, w64 {:.2}x | sums: stack {:.0}, w1 {:.0}, w64 {:.0}",
        stack.len(),
        Bench::median(stack.clone()),
        Bench::median(w1.clone()),
        Bench::median(w64.clone()),
        Bench::geomean(&r1),
        Bench::geomean(&r64),
        stack.iter().sum::<f64>(),
        w1.iter().sum::<f64>(),
        w64.iter().sum::<f64>()
    );
}

struct Scenario {
    source: String,
    origin: &'static str,
    category: &'static str,
    unary: bool,
    scopes: Vec<Scope>,
}

impl Scenario {
    const NAMES: [&'static str; 20] = [
        "anna", "ben", "carla", "dino", "eva", "filip", "greta", "hugo", "ines", "jan", "kira", "lars", "mona", "nils",
        "olga", "petar", "quinn", "rosa", "stefan", "tea",
    ];
    const TIERS: [&'static str; 4] = ["gold", "silver", "bronze", "platinum"];
    const COUNTRIES: [&'static str; 6] = ["US", "DE", "GB", "FR", "HR", "JP"];
    const TAGS: [&'static str; 6] = ["vip", "new", "risk", "promo", "b2b", "trial"];

    fn next(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *state >> 33
    }

    fn row(i: usize) -> Variable {
        let mut r = 0x9e37_79b9_u64.wrapping_mul(i as u64 + 1);
        let mut n = |m: u64| Self::next(&mut r) % m;
        let dec = |m: i64, s: u32| Variable::Number(Decimal::new(m, s));
        let name = format!("{}{}", Self::NAMES[n(20) as usize], n(37));
        let tags: Vec<Variable> = (0..n(5)).map(|_| Variable::String(Self::TAGS[n(6) as usize].into())).collect();
        let nums: Vec<Variable> = (0..n(7)).map(|_| dec(n(100000) as i64 - 20000, 2)).collect();
        let items: Vec<Variable> = (0..n(6))
            .map(|k| {
                let item = Variable::empty_object();
                item.dot_insert("sku", Variable::String(format!("SKU-{k}{}", n(90)).as_str().into()));
                item.dot_insert("qty", dec(n(9) as i64 + 1, 0));
                item.dot_insert("price", dec(n(20000) as i64 + 99, 2));
                item
            })
            .collect();
        let row = Variable::empty_object();
        row.dot_insert("amount", dec(n(1_000_000) as i64, 2));
        row.dot_insert("qty", dec(n(50) as i64 + 1, 0));
        row.dot_insert("price", dec(n(50_000) as i64 + 1, 2));
        row.dot_insert("rate", dec(n(2_000) as i64, 4));
        row.dot_insert("age", dec(n(73) as i64 + 18, 0));
        row.dot_insert("score", dec(n(551) as i64 + 300, 0));
        row.dot_insert("name", Variable::String(name.as_str().into()));
        row.dot_insert("email", Variable::String(format!("{name}@{}.com", Self::TIERS[n(4) as usize]).as_str().into()));
        row.dot_insert("tier", Variable::String(Self::TIERS[n(4) as usize].into()));
        row.dot_insert("country", Variable::String(Self::COUNTRIES[n(6) as usize].into()));
        row.dot_insert("active", Variable::Bool(n(2) == 1));
        row.dot_insert("created", Variable::String(format!("2024-{:02}-{:02}", n(12) + 1, n(28) + 1).as_str().into()));
        row.dot_insert("tags", Variable::from_array(tags));
        row.dot_insert("nums", Variable::from_array(nums));
        row.dot_insert("items", Variable::from_array(items));
        let accounts: Vec<Variable> = (0..n(8))
            .map(|_| {
                let account = Variable::empty_object();
                account.dot_insert("balance", dec(n(500000) as i64, 2));
                account.dot_insert("kind", Variable::String(["savings", "checking", "loan"][n(3) as usize].into()));
                account.dot_insert("active", Variable::Bool(n(4) != 0));
                account
            })
            .collect();
        row.dot_insert("accounts", Variable::from_array(accounts));
        row.dot_insert("customer.age", dec(n(73) as i64 + 18, 0));
        row.dot_insert("customer.country", Variable::String(Self::COUNTRIES[n(6) as usize].into()));
        row.dot_insert("customer.tier", Variable::String(Self::TIERS[n(4) as usize].into()));
        row.dot_insert("customer.address.city", Variable::String(Self::NAMES[n(20) as usize].into()));
        row.dot_insert("customer.address.zip", Variable::String(format!("{:05}", n(99999)).as_str().into()));
        row
    }

    fn synthetic() -> Vec<(&'static str, &'static str, bool)> {
        vec![
            ("access", "amount", false),
            ("access", "customer.address.city", false),
            ("access", "customer.age", false),
            ("access", "items[0].price ?? 0", false),
            ("arithmetic", "amount * 1.2", false),
            ("arithmetic", "qty * price", false),
            ("arithmetic", "(amount - 100) / 3", false),
            ("arithmetic", "round(amount * rate, 2)", false),
            ("arithmetic", "amount % 7", false),
            ("arithmetic", "abs(amount - 5000) + floor(rate * 10)", false),
            ("arithmetic", "qty * price * (1 - rate) + 4.99", false),
            ("logic", "age >= 18 and active", false),
            ("logic", "score > 700 or tier == 'gold'", false),
            ("logic", "amount > 100 and amount < 5000", false),
            ("logic", "not active", false),
            ("logic", "age > 30 ? 'adult' : 'young'", false),
            ("logic", "missing ?? 'none'", false),
            ("logic", "score >= 750 ? 'A' : score >= 650 ? 'B' : score >= 550 ? 'C' : 'D'", false),
            ("membership", "country in ['US', 'DE', 'GB']", false),
            ("membership", "age in [18..65]", false),
            ("membership", "tier not in ['bronze', 'silver']", false),
            ("membership", "score in (500..700]", false),
            ("string", "upper(name)", false),
            ("string", "name + ' ' + tier", false),
            ("string", "startsWith(email, 'a')", false),
            ("string", "contains(name, 'an')", false),
            ("string", "len(name)", false),
            ("string", "lower(tier) == 'gold'", false),
            ("string", "split(email, '@')[1]", false),
            ("string", "matches(email, '^[a-z]+[0-9]*@')", false),
            ("template", "`${name} (${tier})`", false),
            ("template", "`${amount} USD`", false),
            ("date", "d(created).year()", false),
            ("date", "d(created).add(30, 'd').format('%Y-%m-%d')", false),
            ("date", "d(created).isBefore(d('2024-06-01'))", false),
            ("date", "d(created).diff(d('2024-01-01'), 'day')", false),
            ("date", "d(created).startOf('month')", false),
            ("date", "d().diff(d(created), 'day') > 90", false),
            ("closure", "sum(nums)", false),
            ("closure", "map(items, #.qty * #.price)", false),
            ("closure", "sum(map(items, #.qty * #.price))", false),
            ("closure", "filter(tags, # != 'trial')", false),
            ("closure", "some(tags, # == 'vip')", false),
            ("closure", "count(items, #.qty > 2)", false),
            ("closure", "len(tags)", false),
            ("closure", "'vip' in tags", false),
            ("struct", "sum(map(filter(accounts, #.active), #.balance))", false),
            ("struct", "sum(map(accounts, #.balance))", false),
            ("struct", "count(accounts, #.kind == 'loan')", false),
            ("struct", "some(accounts, #.balance > 4000)", false),
            ("struct", "all(accounts, #.active)", false),
            ("struct", "map(accounts, #.kind == 'loan' ? 'L' : 'O')", false),
            ("struct", "sum(map(accounts, #.balance * rate))", false),
            ("struct", "map(accounts, #.balance * 2)", false),
            ("struct", "len(filter(accounts, #.balance > 1000 and #.active))", false),
            ("struct", "sum(map(items, #.qty * #.price))", false),
            ("object", "{total: amount, net: amount * 0.8}", false),
            ("object", "[amount, qty, price]", false),
            ("object", "{name: name, tags: tags, n: len(tags)}", false),
            ("conversion", "string(amount)", false),
            ("conversion", "number(string(qty))", false),
            ("unary", "> 100", true),
            ("unary", "[10..5000]", true),
            ("unary", "'US', 'DE', 'GB'", true),
            ("unary", "startsWith($, 'a')", true),
        ]
    }

    fn unary_reference(row: &Variable, source: &str) -> Variable {
        let key = match source {
            s if s.contains("'US'") => "country",
            s if s.contains("startsWith") => "name",
            _ => "amount",
        };
        row.dot(key).unwrap_or(Variable::Null)
    }

    fn category(source: &str) -> &'static str {
        let s = source;
        if s.contains("d(") || s.contains("date(") || s.contains("time(") {
            return "date";
        }
        if s.contains('#') || ["map(", "filter(", "some(", "all(", "none(", "count(", "sum(", "one("].iter().any(|f| s.contains(f)) {
            return "closure";
        }
        if s.contains('`') {
            return "template";
        }
        if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') {
            return "object";
        }
        if ["upper(", "lower(", "startsWith(", "endsWith(", "contains(", "len(", "trim(", "split(", "matches(", "extract("].iter().any(|f| s.contains(f)) || s.contains("' +") || s.contains("+ '") {
            return "string";
        }
        if s.contains(" in ") {
            return "membership";
        }
        if ["string(", "number(", "bool("].iter().any(|f| s.contains(f)) {
            return "conversion";
        }
        if [" and ", " or ", "==", "!=", ">", "<", "?", "not "].iter().any(|f| s.contains(f)) {
            return "logic";
        }
        if s.chars().any(|c| "+-*/%^".contains(c)) || ["round(", "floor(", "ceil(", "abs(", "max(", "min("].iter().any(|f| s.contains(f)) {
            return "arithmetic";
        }
        "access"
    }

    fn all() -> Vec<Scenario> {
        let mut out: Vec<Scenario> = Bench::cases()
            .into_iter()
            .map(|c| Scenario {
                category: Self::category(&c.source),
                source: c.source,
                origin: "corpus",
                unary: false,
                scopes: c.scopes,
            })
            .collect();
        let rows: Vec<Variable> = (0..ROWS).map(Self::row).collect();
        for (category, source, unary) in Self::synthetic() {
            let scopes = rows
                .iter()
                .map(|row| {
                    let mut scope = Scope::new(row.depth_clone(64));
                    if unary {
                        scope.set_local(Variable::dollar_key(), Self::unary_reference(row, source));
                    }
                    scope
                })
                .collect();
            out.push(Scenario {
                source: source.to_string(),
                origin: "synthetic",
                category,
                unary,
                scopes,
            });
        }
        out
    }
}

fn scenarios() -> bool {
    let Ok(target) = std::env::var("LANE_SCENARIOS") else {
        return false;
    };
    let only = std::env::var("LANE_SCENARIO").ok();
    let category = std::env::var("LANE_CATEGORY").ok();
    let matching = std::env::var("LANE_MATCH").ok();
    let seconds: f64 = std::env::var("LANE_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let budget = Duration::from_millis(std::env::var("LANE_BUDGET_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(60));
    let cases: Vec<Scenario> = Scenario::all()
        .into_iter()
        .filter(|c| category.as_ref().is_none_or(|k| c.category == k))
        .filter(|c| matching.as_ref().is_none_or(|m| c.source.contains(m.as_str())))
        .collect();
    let mut runner = LaneRunner::new();
    struct Prepared<'a> {
        case: &'a Scenario,
        stack: Option<zen_expression::Expression<zen_expression::expression::Standard>>,
        unary: Option<zen_expression::Expression<zen_expression::expression::Unary>>,
        generic: LaneProgram,
        program: LaneProgram,
    }
    let prepared: Vec<Prepared> = cases
        .iter()
        .filter_map(|case| {
            let mut isolate = Isolate::new();
            let (stack, unary, generic) = match case.unary {
                true => (None, Some(isolate.compile_unary(&case.source).ok()?), LaneProgram::unary(&case.source).ok()?),
                false => (Some(isolate.compile_standard(&case.source).ok()?), None, LaneProgram::standard(&case.source).ok()?),
            };
            let program = generic.specialize(&case.scopes).ok()?;
            Some(Prepared { case, stack, unary, generic, program })
        })
        .collect();
    let stack_rows = |p: &Prepared, vm: &mut VM| {
        for scope in &p.case.scopes {
            match (&p.stack, &p.unary) {
                (Some(e), _) => black_box(e.evaluate_with_scope(scope, vm).is_ok()),
                (_, Some(e)) => black_box(e.evaluate_with_scope(scope, vm).is_ok()),
                _ => false,
            };
        }
    };
    let isolate_rows = |p: &Prepared| {
        let mut isolate = Isolate::new();
        for scope in &p.case.scopes {
            isolate.set_environment(scope.base().clone());
            if let Some(reference) = scope.get(&Variable::dollar_key()) {
                isolate.set_local(Variable::dollar_key(), reference.clone());
            }
            match p.case.unary {
                true => black_box(isolate.run_unary(&p.case.source).is_ok()),
                false => black_box(isolate.run_standard(&p.case.source).is_ok()),
            };
        }
    };
    if let Some(only) = only {
        let mut vm = VM::new();
        let tables: Vec<Option<Table>> = prepared.iter().map(|p| (!p.case.unary).then(|| Table::build(&p.case.scopes))).collect();
        let texts: Vec<Vec<(Vec<i32>, Vec<u8>)>> = tables.iter().map(|t| t.as_ref().map(Table::texts).unwrap_or_default()).collect();
        let fields: Vec<Vec<Vec<(&str, Column)>>> = tables.iter().map(|t| t.as_ref().map(Table::fields).unwrap_or_default()).collect();
        let children: Vec<Vec<Column>> = tables
            .iter()
            .zip(texts.iter().zip(&fields))
            .map(|(t, (x, f))| t.as_ref().map(|t| t.children(x, f)).unwrap_or_default())
            .collect();
        let columns: Vec<Option<Columns>> = tables
            .iter()
            .zip(texts.iter().zip(&children))
            .map(|(t, (x, c))| t.as_ref().map(|t| t.columns(x, c)))
            .collect();
        let columnar: Vec<Option<LaneProgram>> = prepared
            .iter()
            .zip(&columns)
            .map(|(p, c)| c.as_ref().and_then(|c| p.generic.specialize_columns(c).ok()))
            .collect();
        let mut out = zen_expression::lane::Output::new();
        let start = Instant::now();
        while start.elapsed().as_secs_f64() < seconds {
            for (i, p) in prepared.iter().enumerate() {
                match only.as_str() {
                    "stack" => stack_rows(p, &mut vm),
                    "isolate" => isolate_rows(p),
                    "lane_single" => {
                        for scope in &p.case.scopes {
                            black_box(runner.evaluate_one(&p.program, scope).is_ok());
                        }
                    }
                    "lane_batch" => {
                        let mut n = 0usize;
                        runner.evaluate_with(&p.program, &p.case.scopes, |_, r| n += r.is_ok() as usize);
                        black_box(n);
                    }
                    "lane_columns" => {
                        if let (Some(program), Some(columns)) = (&columnar[i], &columns[i]) {
                            let mut n = 0usize;
                            runner.evaluate_columns(program, columns, |_, r| n += r.is_ok() as usize);
                            black_box(n);
                        }
                    }
                    "lane_typed" => {
                        if let (Some(program), Some(columns)) = (&columnar[i], &columns[i]) {
                            runner.evaluate_columns_into(program, columns, &mut out);
                            black_box(out.len());
                        }
                    }
                    _ => return true,
                }
            }
        }
        return true;
    }
    let mut lines = Vec::new();
    let mut vm = VM::new();
    let per = |ns: f64| ns / ROWS as f64;
    let compile = |f: &mut dyn FnMut()| {
        let start = Instant::now();
        let mut n = 0u32;
        while start.elapsed() < Duration::from_millis(15) {
            f();
            n += 1;
        }
        start.elapsed().as_nanos() as f64 / n as f64
    };
    for p in &prepared {
        let case = p.case;
        let stack = per(Bench::timed(budget, || stack_rows(p, &mut vm)));
        let isolate = per(Bench::timed(budget, || isolate_rows(p)));
        let single = per(Bench::timed(budget, || {
            for scope in &case.scopes {
                black_box(runner.evaluate_one(&p.program, scope).is_ok());
            }
        }));
        let single_generic = per(Bench::timed(budget, || {
            for scope in &case.scopes {
                black_box(runner.evaluate_one(&p.generic, scope).is_ok());
            }
        }));
        let batch = per(Bench::timed(budget, || {
            let mut n = 0usize;
            runner.evaluate_with(&p.program, &case.scopes, |_, r| n += r.is_ok() as usize);
            black_box(n);
        }));
        let (mut columns_ns, mut typed_ns, mut specialize_columns_ns) = (None, None, None);
        if !case.unary {
            let table = Table::build(&case.scopes);
            let texts = table.texts();
            let fields = table.fields();
            let children = table.children(&texts, &fields);
            let columns = table.columns(&texts, &children);
            if let Ok(columnar) = p.generic.specialize_columns(&columns) {
                columns_ns = Some(per(Bench::timed(budget, || {
                    let mut n = 0usize;
                    runner.evaluate_columns(&columnar, &columns, |_, r| n += r.is_ok() as usize);
                    black_box(n);
                })));
                let mut out = zen_expression::lane::Output::new();
                typed_ns = Some(per(Bench::timed(budget, || {
                    runner.evaluate_columns_into(&columnar, &columns, &mut out);
                    black_box(out.len());
                })));
                specialize_columns_ns = Some(compile(&mut || {
                    black_box(p.generic.specialize_columns(&columns).is_ok());
                }));
            }
        }
        let stack_compile = compile(&mut || {
            let mut isolate = Isolate::new();
            match case.unary {
                true => black_box(isolate.compile_unary(&case.source).is_ok()),
                false => black_box(isolate.compile_standard(&case.source).is_ok()),
            };
        });
        let lane_compile = compile(&mut || {
            match case.unary {
                true => black_box(LaneProgram::unary(&case.source).is_ok()),
                false => black_box(LaneProgram::standard(&case.source).is_ok()),
            };
        });
        let specialize = compile(&mut || {
            black_box(p.generic.specialize(&case.scopes[..64]).is_ok());
        });
        let errors = case
            .scopes
            .iter()
            .filter(|scope| match (&p.stack, &p.unary) {
                (Some(e), _) => e.evaluate_with_scope(scope, &mut vm).is_err(),
                (_, Some(e)) => e.evaluate_with_scope(scope, &mut vm).is_err(),
                _ => false,
            })
            .count();
        let opt = |v: Option<f64>| v.map_or(Value::Null, |v| serde_json::json!(v));
        lines.push(serde_json::json!({
            "source": case.source,
            "origin": case.origin,
            "category": case.category,
            "unary": case.unary,
            "error_rows": errors,
            "ns": {
                "stack": stack,
                "isolate": isolate,
                "lane_single": single,
                "lane_single_generic": single_generic,
                "lane_batch": batch,
                "lane_columns": opt(columns_ns),
                "lane_typed": opt(typed_ns),
            },
            "compile_ns": {
                "stack": stack_compile,
                "lane": lane_compile,
                "specialize_rows": specialize,
                "specialize_columns": opt(specialize_columns_ns),
            },
        }));
        eprintln!("{:10} {:9} stack {stack:7.1} single {single:7.1} batch {batch:7.1} columns {:7.1} | {}", case.category, case.origin, columns_ns.unwrap_or(0.0), &case.source[..case.source.len().min(60)]);
    }
    let _ = std::fs::write(&target, serde_json::to_string(&lines).unwrap_or_default());
    true
}

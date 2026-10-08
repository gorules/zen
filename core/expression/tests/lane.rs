mod conformance;

use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::ops::Index;
use zen_expression::lane::{Column, Columns, Fusion, LaneProgram, LaneRunner, Output, Values};
use zen_expression::variable::VariableType;
use zen_expression::{ExpressionKind, Isolate, Scope, Variable};

struct Case {
    expression: String,
    input: Value,
}

struct Differential {
    kind: ExpressionKind,
    runner: LaneRunner,
    failures: Vec<String>,
    compared: usize,
    outcomes: [usize; 3],
}

impl Differential {
    fn new(kind: ExpressionKind) -> Self {
        std::env::set_var(
            "TZ",
            std::env::var("LANE_TZ").unwrap_or_else(|_| "UTC".to_string()),
        );
        std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-03-20T10:15:30Z");
        Self {
            kind,
            runner: LaneRunner::new(),
            failures: Vec::new(),
            compared: 0,
            outcomes: [0; 3],
        }
    }

    fn cases(csv_data: &str) -> Vec<Case> {
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(b';')
            .from_reader(csv_data.as_bytes());
        reader
            .records()
            .flatten()
            .filter(|row| !row.index(0).starts_with('#'))
            .map(|row| Case {
                expression: row.index(0).to_string(),
                input: match row.index(1) {
                    "" => Value::Null,
                    s => serde_json5::from_str(s).unwrap_or(Value::Null),
                },
            })
            .collect()
    }

    fn stack(&self, expression: &str, input: &Value) -> Result<Value, String> {
        match input.is_null() {
            true => self.stack_of(expression, None),
            false => self.stack_of(expression, Some(input.clone().into())),
        }
    }

    fn stack_of(&self, expression: &str, input: Option<Variable>) -> Result<Value, String> {
        let mut isolate = Isolate::new();
        if let Some(input) = input {
            isolate.set_environment(input);
        }
        match self.kind {
            ExpressionKind::Standard => isolate
                .run_standard(expression)
                .map(|v| v.to_value())
                .map_err(|e| e.to_string()),
            ExpressionKind::Unary => isolate
                .run_unary(expression)
                .map(Value::Bool)
                .map_err(|e| e.to_string()),
        }
    }

    fn lane(
        &self,
        result: Result<Variable, zen_expression::IsolateError>,
    ) -> Result<Value, String> {
        match (self.kind.clone(), result) {
            (ExpressionKind::Standard, r) => r.map(|v| v.to_value()).map_err(|e| e.to_string()),
            (ExpressionKind::Unary, Ok(v)) => v
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| zen_expression::IsolateError::ValueCastError.to_string()),
            (ExpressionKind::Unary, Err(e)) => Err(e.to_string()),
        }
    }

    fn scope(input: &Value) -> Scope {
        match input {
            Value::Null => Scope::default(),
            v => Scope::new(v.clone().into()),
        }
    }

    fn check(&mut self, expression: &str, inputs: &[Value]) {
        let program = LaneProgram::compile(expression, self.kind.clone());
        let expected: Vec<Result<Value, String>> =
            inputs.iter().map(|i| self.stack(expression, i)).collect();
        for exp in &expected {
            match exp {
                Ok(_) => self.outcomes[0] += 1,
                Err(_) => self.outcomes[1] += 1,
            }
        }
        let program = match program {
            Ok(p) => p,
            Err(e) => {
                self.outcomes[2] += 1;
                for (input, exp) in inputs.iter().zip(&expected) {
                    self.compared += 1;
                    if exp.as_ref().err() != Some(&e.to_string()) {
                        self.failures.push(format!(
                            "{expression} | input {input} | compile: lane {e} vs stack {exp:?}"
                        ));
                    }
                }
                return;
            }
        };

        let scopes: Vec<Scope> = inputs.iter().map(Self::scope).collect();
        if let Some(first) = inputs.first() {
            let typed = LaneProgram::compile_typed(
                expression,
                self.kind.clone(),
                &VariableType::from(first),
            )
            .expect("typed compile");
            let batch = self.runner.evaluate(&typed, &scopes);
            for ((input, exp), got) in inputs.iter().zip(&expected).zip(batch) {
                self.compared += 1;
                let got = self.lane(got);
                if &got != exp {
                    self.failures.push(format!(
                        "{expression} | input {input} | typed from first input | lane {got:?} vs stack {exp:?}"
                    ));
                }
            }
        }
        let specialized = program.specialize(&scopes).expect("specialize");
        for (input, exp) in inputs.iter().zip(&expected) {
            let got = self.runner.evaluate_one(&specialized, &Self::scope(input));
            let got = self.lane(got);
            self.compared += 1;
            if &got != exp {
                self.failures.push(format!(
                    "{expression} | input {input} | specialized | lane {got:?} vs stack {exp:?}"
                ));
            }
        }
        let batch = self.runner.evaluate(&specialized, &scopes);
        for ((input, exp), got) in inputs.iter().zip(&expected).zip(batch) {
            self.compared += 1;
            let got = self.lane(got);
            if &got != exp {
                self.failures.push(format!(
                    "{expression} | input {input} | specialized batch | lane {got:?} vs stack {exp:?}"
                ));
            }
        }
        let batch = self.runner.evaluate(&program, &scopes);
        for ((input, exp), got) in inputs.iter().zip(&expected).zip(batch) {
            self.compared += 1;
            let got = self.lane(got);
            if &got != exp {
                self.failures.push(format!(
                    "{expression} | input {input} | width {} | lane {got:?} vs stack {exp:?}",
                    inputs.len().min(64)
                ));
            }
        }

        for (input, exp) in inputs.iter().zip(&expected) {
            let got = self.runner.evaluate_one(&program, &Self::scope(input));
            let got = self.lane(got);
            self.compared += 1;
            if &got != exp {
                self.failures.push(format!(
                    "{expression} | input {input} | width 1 | lane {got:?} vs stack {exp:?}"
                ));
            }
        }
    }

    fn columns(&mut self, expression: &str, inputs: &[Value]) {
        let Ok(program) = LaneProgram::compile(expression, self.kind.clone()) else {
            return;
        };
        let inputs: Vec<Value> = inputs.iter().cycle().take(inputs.len().max(72)).cloned().collect();
        let inputs = inputs.as_slice();
        let owned = Owned::build(inputs);
        let texts: Vec<Vec<&str>> = owned
            .texts
            .iter()
            .map(|t| t.iter().map(String::as_str).collect())
            .collect();
        let children = owned.children(&texts);
        let columns = owned.columns(&texts, &children);
        let rows: Vec<Value> = (0..inputs.len())
            .map(|r| columns.row(r).to_value())
            .collect();
        let expected: Vec<Result<Value, String>> = (0..inputs.len())
            .map(|r| self.stack_of(expression, Some(columns.row(r))))
            .collect();
        for program in [
            program.clone(),
            program.specialize_columns(&columns).expect("specialize"),
        ] {
            let mut raw = Vec::new();
            self.runner
                .evaluate_columns(&program, &columns, |i, r| raw.push((i, r)));
            let mut got = vec![Err(String::new()); inputs.len()];
            for (i, r) in raw {
                got[i] = self.lane_of(r);
            }
            for ((row, exp), got) in rows.iter().zip(&expected).zip(got) {
                self.compared += 1;
                if &got != exp {
                    self.failures.push(format!(
                        "{expression} | row {row} | columns | lane {got:?} vs stack {exp:?}"
                    ));
                }
            }
            let mut out = Output::new();
            self.runner
                .evaluate_columns_into(&program, &columns, &mut out);
            for (i, (row, exp)) in rows.iter().zip(&expected).enumerate() {
                self.compared += 1;
                let got = match out.variable(i) {
                    Some(r) => self.lane_of(
                        r.map_err(|source| zen_expression::IsolateError::VMError { source }),
                    ),
                    None => Err("missing row".to_string()),
                };
                if &got != exp {
                    self.failures.push(format!(
                        "{expression} | row {row} | columns-out | lane {got:?} vs stack {exp:?}"
                    ));
                }
            }
        }
    }

    fn lane_of(
        &self,
        result: Result<Variable, zen_expression::IsolateError>,
    ) -> Result<Value, String> {
        self.lane(result)
    }

    fn file(&mut self, csv_data: &str) {
        let cases = Self::cases(csv_data);
        let mut inputs: Vec<Value> = Vec::new();
        for case in &cases {
            if !inputs.contains(&case.input) {
                inputs.push(case.input.clone());
            }
        }
        for case in &cases {
            self.check(&case.expression, std::slice::from_ref(&case.input));
            self.check(&case.expression, &inputs);
            self.columns(&case.expression, &inputs);
        }
    }

    fn assert(self) {
        eprintln!(
            "compared {} | expected ok {} err {} | compile errors {}",
            self.compared, self.outcomes[0], self.outcomes[1], self.outcomes[2]
        );
        assert!(self.compared > 0);
        let shown: Vec<&String> = self.failures.iter().take(40).collect();
        assert!(
            self.failures.is_empty(),
            "{} of {} comparisons differ:\n{}",
            self.failures.len(),
            self.compared,
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

struct Owned {
    rows: usize,
    keys: Vec<String>,
    data: Vec<OwnedValues>,
    texts: Vec<Vec<String>>,
    valid: Vec<Vec<u64>>,
}

enum OwnedValues {
    I64(Vec<i64>),
    Dec(Vec<Decimal>),
    Bool(Vec<u64>),
    Str(usize),
    Utf8(Vec<i32>, Vec<u8>),
    Dict(Vec<i32>, usize),
    List(Vec<i32>, OwnedChild),
    Any(Vec<Variable>),
}

enum OwnedChild {
    I64(Vec<i64>),
    Dec(Vec<Decimal>),
    Str(usize),
}

impl Owned {
    fn flatten(value: &Value, prefix: String, out: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(map) if !map.is_empty() => {
                for (k, v) in map {
                    let path = if prefix.is_empty() {
                        k.clone()
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

    fn build(inputs: &[Value]) -> Self {
        let flat: Vec<BTreeMap<String, Value>> = inputs
            .iter()
            .map(|v| {
                let mut m = BTreeMap::new();
                Self::flatten(v, String::new(), &mut m);
                m
            })
            .collect();
        let mut keys: Vec<String> = flat.iter().flat_map(|m| m.keys().cloned()).collect();
        keys.sort();
        keys.dedup();
        let rows = inputs.len();
        let mut data = Vec::new();
        let mut texts = Vec::new();
        let mut valid = Vec::new();
        for key in &keys {
            let values: Vec<Option<&Value>> = flat.iter().map(|m| m.get(key)).collect();
            let mut bits = vec![0u64; rows.div_ceil(64).max(1)];
            for (i, v) in values.iter().enumerate() {
                if v.is_some_and(|v| !v.is_null()) {
                    bits[i / 64] |= 1 << (i % 64);
                }
            }
            let present: Vec<&Value> = values
                .iter()
                .flatten()
                .copied()
                .filter(|v| !v.is_null())
                .collect();
            let all = |f: fn(&Value) -> bool| !present.is_empty() && present.iter().all(|v| f(v));
            let column = if all(|v| v.as_i64().is_some() && !v.to_string().contains('.')) {
                OwnedValues::I64(
                    values
                        .iter()
                        .map(|v| v.and_then(Value::as_i64).unwrap_or(0))
                        .collect(),
                )
            } else if all(Value::is_number) {
                OwnedValues::Dec(
                    values
                        .iter()
                        .map(|v| match v.map(|v| Variable::from(v.clone())) {
                            Some(Variable::Number(n)) => n,
                            _ => Decimal::ZERO,
                        })
                        .collect(),
                )
            } else if all(Value::is_boolean) {
                let mut words = vec![0u64; rows.div_ceil(64).max(1)];
                for (i, v) in values.iter().enumerate() {
                    if v.and_then(Value::as_bool) == Some(true) {
                        words[i / 64] |= 1 << (i % 64);
                    }
                }
                OwnedValues::Bool(words)
            } else if all(Value::is_string) && key.len() % 2 == 1 {
                let mut distinct: Vec<String> = Vec::new();
                let codes = values
                    .iter()
                    .map(|v| {
                        let text = v.and_then(Value::as_str).unwrap_or_default();
                        match distinct.iter().position(|d| d == text) {
                            Some(i) => i as i32,
                            None => {
                                distinct.push(text.to_string());
                                (distinct.len() - 1) as i32
                            }
                        }
                    })
                    .collect();
                texts.push(distinct);
                OwnedValues::Dict(codes, texts.len() - 1)
            } else if all(Value::is_string) && key.len() % 4 == 0 {
                let mut offsets = vec![0i32];
                let mut bytes = Vec::new();
                for v in &values {
                    bytes.extend_from_slice(
                        v.and_then(Value::as_str).unwrap_or_default().as_bytes(),
                    );
                    offsets.push(bytes.len() as i32);
                }
                OwnedValues::Utf8(offsets, bytes)
            } else if all(Value::is_string) {
                texts.push(
                    values
                        .iter()
                        .map(|v| v.and_then(Value::as_str).unwrap_or_default().to_string())
                        .collect(),
                );
                OwnedValues::Str(texts.len() - 1)
            } else if all(|v| v.as_array().is_some_and(|a| a.iter().all(Value::is_number)))
                || all(|v| v.as_array().is_some_and(|a| a.iter().all(Value::is_string)))
            {
                let numbers = present
                    .iter()
                    .all(|v| v.as_array().is_some_and(|a| a.iter().all(Value::is_number)));
                let mut offsets = vec![0i32];
                let mut items: Vec<&Value> = Vec::new();
                for v in &values {
                    if let Some(array) = v.and_then(Value::as_array) {
                        items.extend(array.iter());
                    }
                    offsets.push(items.len() as i32);
                }
                let ints = items
                    .iter()
                    .all(|v| v.as_i64().is_some() && !v.to_string().contains('.'));
                let child = match numbers {
                    true if ints && key.len() % 2 == 0 => {
                        OwnedChild::I64(items.iter().filter_map(|v| v.as_i64()).collect())
                    }
                    true => OwnedChild::Dec(
                        items
                            .iter()
                            .map(|v| match Variable::from((*v).clone()) {
                                Variable::Number(n) => n,
                                _ => Decimal::ZERO,
                            })
                            .collect(),
                    ),
                    false => {
                        texts.push(
                            items
                                .iter()
                                .map(|v| v.as_str().unwrap_or_default().to_string())
                                .collect(),
                        );
                        OwnedChild::Str(texts.len() - 1)
                    }
                };
                OwnedValues::List(offsets, child)
            } else {
                OwnedValues::Any(
                    values
                        .iter()
                        .map(|v| v.map_or(Variable::Null, |v| Variable::from(v.clone())))
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
            texts,
            valid,
        }
    }

    fn children<'a>(&'a self, texts: &'a [Vec<&'a str>]) -> Vec<Column<'a>> {
        self.data
            .iter()
            .map(|data| match data {
                OwnedValues::Dict(_, i) => Column::new(Values::Strs(&texts[*i])),
                OwnedValues::List(_, OwnedChild::I64(v)) => Column::new(Values::I64(v)),
                OwnedValues::List(_, OwnedChild::Dec(v)) => Column::new(Values::Dec(v)),
                OwnedValues::List(_, OwnedChild::Str(i)) => Column::new(Values::Strs(&texts[*i])),
                _ => Column::new(Values::Any(&[])),
            })
            .collect()
    }

    fn columns<'a>(&'a self, texts: &'a [Vec<&'a str>], children: &'a [Column<'a>]) -> Columns<'a> {
        let mut columns = Columns::new(self.rows);
        for (((key, data), valid), child) in self
            .keys
            .iter()
            .zip(&self.data)
            .zip(&self.valid)
            .zip(children)
        {
            let values = match data {
                OwnedValues::I64(v) => Values::I64(v),
                OwnedValues::Dec(v) => Values::Dec(v),
                OwnedValues::Bool(bits) => Values::Bool { bits, offset: 0 },
                OwnedValues::Str(i) => Values::Strs(&texts[*i]),
                OwnedValues::Utf8(offsets, data) => Values::Utf8 { offsets, data },
                OwnedValues::Dict(keys, _) => Values::Dict {
                    keys,
                    values: child.into(),
                },
                OwnedValues::List(offsets, _) => Values::List { offsets, child: child.into() },
                OwnedValues::Any(v) => Values::Any(v),
            };
            columns = columns.column(key, Column::with_validity(values, valid, 0));
        }
        columns
    }
}

#[test]
fn lane_matches_stack_vm_standard_csv() {
    let mut d = Differential::new(ExpressionKind::Standard);
    d.file(include_str!("data/standard.csv"));
    d.assert();
}

#[test]
fn lane_matches_stack_vm_date_csv() {
    let mut d = Differential::new(ExpressionKind::Standard);
    d.file(include_str!("data/date.csv"));
    d.assert();
}

#[test]
fn lane_matches_stack_vm_unary_csv() {
    let mut d = Differential::new(ExpressionKind::Unary);
    d.file(include_str!("data/unary.csv"));
    d.assert();
}

#[test]
fn lane_struct_outputs_stay_typed() {
    let inputs = vec![
        json!({"a": 1, "s": "x"}),
        json!({"a": 2.5, "s": "yy"}),
        json!({"a": "bad", "s": null}),
    ];
    let owned = Owned::build(&inputs);
    let texts: Vec<Vec<&str>> = owned
        .texts
        .iter()
        .map(|t| t.iter().map(String::as_str).collect())
        .collect();
    let children = owned.children(&texts);
    let columns = owned.columns(&texts, &children);
    let program = LaneProgram::compile("{n: a * 2, t: s, p: [a, a + 1]}", ExpressionKind::Standard)
        .expect("compile")
        .specialize_columns(&columns)
        .expect("specialize");
    let mut runner = LaneRunner::new();
    let mut out = Output::new();
    runner.evaluate_columns_into(&program, &columns, &mut out);
    let fields = out.fields();
    assert_eq!(
        fields.iter().map(|(k, _)| k.as_ref()).collect::<Vec<_>>(),
        ["n", "t", "p"]
    );
    assert_eq!(fields[0].1.kind(), zen_expression::lane::Kind::Num);
    assert_eq!(fields[1].1.kind(), zen_expression::lane::Kind::Str);
    assert_eq!(fields[0].1.mantissas()[..2], [2, 50]);
    assert_eq!(fields[0].1.scales()[..2], [0, 1]);
    assert_eq!(fields[1].1.text(1), Some("yy"));
    let list = fields[2].1.child().expect("list");
    assert_eq!(list.kind(), zen_expression::lane::Kind::Num);
    assert_eq!(fields[2].1.offsets()[..3], [0, 2, 4]);
    assert_eq!(list.mantissas()[..4], [1, 2, 25, 35]);
    assert_eq!(list.scales()[..4], [0, 0, 1, 1]);
    assert!(out.failed()[0] >> 2 & 1 == 1);
}

#[test]
fn lane_matches_stack_vm_special_cases() {
    let mut d = Differential::new(ExpressionKind::Standard);
    let inputs = vec![
        json!({}),
        json!({"a": 1, "b": [1, 2, 3], "s": "abc", "o": {"x": 1}}),
        json!({"a": "x", "b": null, "s": 5}),
        Value::Null,
        json!({"s": "abc", "i": 18446744073709551615u64}),
    ];
    let expressions = [
        "r = map([1,2,3] as x, (y = x * 2; y + 1)); r",
        "r = flatMap([[1],[2,3]] as x, (y = x; y)); r",
        "r = map([1,2] as x, map([10,20] as z, (y = x * z; y))); r",
        "a = map([1] as x, (y = x; y)); b = a[0] + 1; b",
        "d('2020-01-01').add(10000000000000000, 's').isValid()",
        "s[i]",
        "79228162514264337593543950335 + 1",
        "sum([79228162514264337593543950335, 79228162514264337593543950335])",
        "1 / 0",
        "1 % 0",
        "#",
        "map(b, # + a)",
        "map(b, map(b as y, # + y))",
        "filter(b, # > 1)",
        "some(b, # == 'x')",
        "all(b, # > 0)",
        "none(b, #)",
        "one(b, # == 2)",
        "count(b, # > 1)",
        "a and b",
        "a or b",
        "a ?? b ?? 3",
        "a ? 1 : 2",
        "$root",
        "$root.a",
        "x = 1; y = x + a; {x, y}",
        "o.x",
        "o['x']",
        "b[1]",
        "b[1:2]",
        "s[1:]",
        "`${a} and ${s}`",
        "{a: 1, [s]: 2}",
        "{x: a, y: s, z: {w: a + 1, v: [a, a * 2]}}",
        "{x: a, x: s}",
        "{x: 1 / 0, y: a}",
        "{x: a.y.z, y: len(b)}",
        "[a, s, o]",
        "[a, a, a + 1]",
        "[s, s]",
        "[]",
        "{}",
        "[{x: a}, {x: s}]",
        "{items: map(b, # * 2), n: count(b, # > 1), d: d('2024-01-01')}",
        "{c: s == 'abc', t: [true, a == 1]}",
        "{x: 'lit', y: 'lit', z: 1}",
        "[1, 1, 'q']",
        "max([a, 2])",
        "min([a, 2, -1.5])",
        "max([1, 1.0, 1.00])",
        "min([1.00, 1.0, 1])",
        "max([a, s])",
        "min([a])",
        "max([a * 2, a + 1, 0.5])",
        "sum(b)",
        "avg(b)",
        "min(b)",
        "max(b)",
        "sum(map(b, # * 1.5))",
        "avg(map(b, # / 3))",
        "max(map(b, # * -1))",
        "min(filter(b, # > 1))",
        "sum(filter(b, # > 5))",
        "sum(map(b, # * 0.5 - 1))",
        "sum(map(b, 0.5 - # * 0.5))",
        "sum(map(b, # * 0.10))",
        "max(map(b, # * 0.5))",
        "min(map(b, # * 1.0 - 2))",
        "map(filter(b, # > 1), # * 0.5)",
        "max(filter(b, # > 99))",
        "min(filter(b, # > 99))",
        "max(map(b, 'x' + string(#)))",
        "min(map(b, d('2024-01-01').add(#, 'd')))",
        "max(map(b, # > 1))",
        "max(map(b, # * 0.5)) + 1",
        "len(map(b, #))",
        "{x: map(b, # * 2), y: filter(b, # > 1), z: count(b, # > 1)}",
        "map(b, [#, # * 2])",
        "map(b, {v: #})",
        "filter(b, # > 1) == [2, 3]",
        "a in [1, 2]",
        "a not in [1, 2]",
        "a in [0..5]",
        "not a",
        "-a",
        "a ^ 2",
        "a == b",
        "a != b",
        "len(b)",
        "some(b, # == 2) and all(b, # > 0)",
        "map(1..3, # * 2)",
        "map(b, x = #; x)",
        "a * 7.40 + a * 7.4",
        "(a ? 1.50 : 1.5) + 0.0",
        "[1.0, 1.00, 1] == [1, 1, 1]",
        "a in [2.0, 2.00]",
        "map([1, 2, 3] as y, map([{x: 1}], o[#.x] in [y, #]))",
        "v = a; w = all(['x', 'y'], # == 'y' and v = #; true); v",
        "a in [1..10]",
        "a in (1..10)",
        "a in ]1..10[",
        "a in [1..10)",
        "a not in [2..3]",
        "s in [1..10]",
        "zz in [1..10]",
        "b in [10..1]",
        "date(s) in [1..2]",
        "map(b, # in [1.5..2.50])",
        "a == 1 or a == 2 or a == 3",
        "s == 'abc' or (s == 'x' or s == null)",
        "b == 1 or b == 2",
        "zz == null or zz == 1",
        "a == 1 or s == 'abc'",
        "map(b, # == 2 or # == 3)",
        "a == 1.0 or a == 2.00",
        "a * 1.5 + a > 10",
        "(a - 2) * 3 / 4",
        "a / (a - a) + 1",
        "a % (a - a) * 2",
        "zz * 2 + 1",
        "s * 2 + 1",
        "a * 2 + s > 1",
        "2 * 3 + a",
        "(2 * 3) + (4 / 8) < a",
        "a * 79228162514264337593543950335 + 1",
        "a - 79228162514264337593543950335 * 2 > 0",
        "a + (a * 1 + (a * 2 + (a * 3 + (a * 4 + (a * 5 + (a * 6 + (a * 7 + (a * 8 + (a * 9 + (a * 10 + (a * 11 + (a * 12 + (a * 13 + (a * 14 + (a * 15 + (a * 16 + (a * 17 + (a * 18 + (a * 19)))))))))))))))))))",
        "map(b, # * 2 + 1 > 3)",
        "a * 1.10 + 0.90",
        "a / 3 * 3 == a",
        "a == 1 and b > 2",
        "a == 1 and s == 'abc' and 5 < a",
        "a != 1 or s == 'abc' or b >= 3",
        "(a == 1 and (s == 'abc' and zz == null))",
        "s > 1 and a == 1",
        "a == 1 and s > 1",
        "a == 2 or s > 1",
        "s > 1 or a == 2",
        "map(b, # > 1 and # < 3)",
        "a == 1 and b > 2 or s == 'x'",
        "a * 1.5 + a - 0.25",
        "a * 0 + 0.000 - a",
        "(a - a) + (a - a) * 1.00",
        "0.000 - (a - a)",
        "a * 3.10 - a * 3.1 > 0",
        "a * 1.25 + a * 2 >= 7.5",
        "a * 922337203685477580 + a * 922337203685477580",
        "a * 0.0000000000000000000001 * 0.00000001 + 1",
        "a - 9223372036854775807 - 9223372036854775807 < 0",
        "zz * 2 + 1",
        "s * 2 + a",
        "a + zz * 2 > 1",
        "x = 3; x * a + 1",
        "(0 - (a - a) * 1.00) * 2 + a * 0.50",
        "(-0.00) * a + a - 0.5 * a",
        "len(s)",
        "contains(s, 'b')",
        "contains(s, 1)",
        "startsWith(s, 'a')",
        "endsWith(s, 'c')",
        "upper(s) + lower(s) + trim(s)",
        "len(zz)",
        "lower(a)",
        "sum(b)",
        "avg(b)",
        "min(b)",
        "max(b)",
        "len(b)",
        "contains(b, 2)",
        "contains(b, 'x')",
        "sum(zz)",
        "date(s)",
        "v = a; w = some(['x', 'y'], # == 'x' or v = #; false); v",
    ];
    for e in expressions {
        d.columns(e, &inputs);
        d.check(e, &inputs);
        for input in &inputs {
            d.check(e, std::slice::from_ref(input));
        }
    }
    d.assert();
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

struct Generator<'r> {
    rng: &'r mut Rng,
    closures: usize,
    aliases: Vec<&'static str>,
}

impl Generator<'_> {
    const FIELDS: [&'static str; 14] = [
        "a", "b", "c", "s", "t", "flag", "items", "tags", "o", "o.x", "o.y.z", "missing", "n",
        "list",
    ];
    const FUNCTIONS: [(&'static str, usize); 22] = [
        ("len", 1),
        ("abs", 1),
        ("round", 1),
        ("floor", 1),
        ("ceil", 1),
        ("sum", 1),
        ("min", 1),
        ("max", 1),
        ("avg", 1),
        ("median", 1),
        ("contains", 2),
        ("upper", 1),
        ("lower", 1),
        ("trim", 1),
        ("startsWith", 2),
        ("endsWith", 2),
        ("string", 1),
        ("number", 1),
        ("keys", 1),
        ("values", 1),
        ("isNumeric", 1),
        ("flatten", 1),
    ];

    fn leaf(&mut self) -> String {
        match self.rng.below(12) {
            0 => format!("{}", self.rng.below(20) as i64 - 5),
            1 => format!("{}.{}", self.rng.below(100), self.rng.below(100)),
            2 => format!(
                "'{}'",
                self.rng.pick(&["x", "gold", "abc", "", "1", "Hello"])
            ),
            3 => self.rng.pick(&["true", "false", "null"]).to_string(),
            4 if self.closures > 0 => "#".to_string(),
            5 if !self.aliases.is_empty() => {
                let i = self.rng.below(self.aliases.len());
                self.aliases[i].to_string()
            }
            6 if self.closures > 0 => format!("#.{}", self.rng.pick(&["x", "v", "name"])),
            _ => self.rng.pick(&Self::FIELDS).to_string(),
        }
    }

    fn list(&mut self, depth: usize) -> String {
        match self.rng.below(6) {
            0 => "[1, 2, 3]".to_string(),
            1 => "tags".to_string(),
            2 => "list".to_string(),
            3 => "1..4".to_string(),
            4 => self.expr(depth),
            _ => "items".to_string(),
        }
    }

    fn expr(&mut self, depth: usize) -> String {
        if depth == 0 || self.rng.chance(20) {
            return self.leaf();
        }
        let d = depth - 1;
        match self.rng.below(20) {
            0..=3 => {
                let op = self.rng.pick(&[
                    "+", "-", "*", "/", "%", "^", "==", "!=", "<", "<=", ">", ">=", "and", "or",
                    "??",
                ]);
                format!("{} {op} {}", self.expr(d), self.expr(d))
            }
            4 => format!("({} ? {} : {})", self.expr(d), self.expr(d), self.expr(d)),
            5 => format!("{}{}", self.rng.pick(&["-", "not ", "+"]), self.expr(d)),
            6 => {
                let (name, arity) = Self::FUNCTIONS[self.rng.below(Self::FUNCTIONS.len())];
                let args: Vec<String> = (0..arity).map(|_| self.expr(d)).collect();
                format!("{name}({})", args.join(", "))
            }
            7 | 8 => {
                let kind = self.rng.pick(&[
                    "map", "filter", "some", "all", "none", "one", "count", "flatMap",
                ]);
                let list = self.list(d);
                self.closures += 1;
                let alias = self.rng.chance(20).then(|| self.rng.pick(&["x", "y"]));
                let body = match alias {
                    Some(a) => {
                        self.aliases.push(a);
                        let b = self.expr(d);
                        self.aliases.pop();
                        format!("{list} as {a}, {b}")
                    }
                    None => format!("{list}, {}", self.expr(d)),
                };
                self.closures -= 1;
                format!("{kind}({body})")
            }
            9 => format!("[{}, {}]", self.expr(d), self.expr(d)),
            10 => format!("{{k: {}, 'm': {}}}", self.expr(d), self.expr(d)),
            11 => format!("{} in [{}, {}]", self.expr(d), self.leaf(), self.leaf()),
            12 => format!(
                "{} in [{}..{}]",
                self.expr(d),
                self.rng.below(5),
                5 + self.rng.below(5)
            ),
            13 => format!("`${{{}}}-${{{}}}`", self.expr(d), self.expr(d)),
            14 => format!(
                "{}[{}]",
                self.rng.pick(&["items", "tags", "s", "o", "list"]),
                self.expr(d)
            ),
            15 => format!(
                "{}[{}:{}]",
                self.rng.pick(&["items", "s", "tags"]),
                self.rng.below(3),
                self.rng.below(4)
            ),
            16 => format!(
                "v = {}; w = {}; {}",
                self.expr(d),
                self.expr(d),
                self.rng.pick(&["v", "w", "v + w", "{v, w}"])
            ),
            17 => format!("{} not in [{}]", self.expr(d), self.leaf()),
            18 => format!("o.{}", self.rng.pick(&["x", "y", "y.z", "missing"])),
            _ => self.leaf(),
        }
    }

    fn value(rng: &mut Rng) -> Value {
        match rng.below(9) {
            0 => json!(rng.below(20) as i64 - 5),
            1 => json!(format!("{}.{}", rng.below(50), rng.below(100))
                .parse::<f64>()
                .unwrap_or(0.0)),
            2 => json!(["x", "gold", "abc", "1", ""][rng.below(5)]),
            3 => json!(rng.chance(50)),
            4 => Value::Null,
            5 => json!([1, 2, 3]),
            6 => json!({"x": rng.below(5), "v": "a"}),
            _ => json!(rng.below(100)),
        }
    }

    fn input(rng: &mut Rng) -> Value {
        let mut o = serde_json::Map::new();
        let typed = rng.chance(60);
        let num = |rng: &mut Rng| {
            if typed {
                json!(rng.below(30) as i64 - 10)
            } else {
                Self::value(rng)
            }
        };
        for k in ["a", "b", "c", "n"] {
            if !rng.chance(10) {
                o.insert(k.into(), num(rng));
            }
        }
        for k in ["s", "t"] {
            if !rng.chance(10) {
                let v = if typed {
                    json!(["x", "gold", "abc", "Hello"][rng.below(4)])
                } else {
                    Self::value(rng)
                };
                o.insert(k.into(), v);
            }
        }
        if !rng.chance(10) {
            let v = if typed {
                json!(rng.chance(50))
            } else {
                Self::value(rng)
            };
            o.insert("flag".into(), v);
        }
        let len = rng.below(6);
        let items: Vec<Value> = (0..len)
            .map(|_| {
                if typed {
                    json!(rng.below(10))
                } else {
                    Self::value(rng)
                }
            })
            .collect();
        o.insert("items".into(), Value::Array(items));
        let tags: Vec<Value> = (0..rng.below(4))
            .map(|_| json!(["x", "gold", "abc"][rng.below(3)]))
            .collect();
        o.insert("tags".into(), Value::Array(tags));
        let list: Vec<Value> = (0..rng.below(4))
            .map(|_| {
                let v = ["a", "b"][rng.below(2)];
                json!({"x": rng.below(10), "v": v, "name": "n"})
            })
            .collect();
        o.insert("list".into(), Value::Array(list));
        if !rng.chance(15) {
            o.insert(
                "o".into(),
                json!({"x": rng.below(10), "y": {"z": rng.below(10)}}),
            );
        }
        Value::Object(o)
    }
}

#[test]
fn lane_fuzz_matches_stack_vm() {
    let iterations: u64 = std::env::var("LANE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let seed: u64 = std::env::var("LANE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5EED);
    let mut d = Differential::new(ExpressionKind::Standard);
    for i in 0..iterations {
        let mut rng = Rng(seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(i.wrapping_mul(2654435761).wrapping_add(1)));
        let expression = Generator {
            rng: &mut rng,
            closures: 0,
            aliases: Vec::new(),
        }
        .expr(4);
        let inputs: Vec<Value> = (0..48).map(|_| Generator::input(&mut rng)).collect();
        let before = d.failures.len();
        d.check(&expression, &inputs);
        d.columns(&expression, &inputs);
        if d.failures.len() > before {
            eprintln!("fuzz case {i} (seed {seed}) failed: {expression}");
        }
    }
    d.assert();
}

struct Typed<'r> {
    rng: &'r mut Rng,
    closure: bool,
}

impl Typed<'_> {
    fn num(&mut self, depth: usize) -> String {
        if depth == 0 || self.rng.chance(25) {
            return match self.rng.below(6) {
                0 => format!("{}", self.rng.below(50) as i64 - 10),
                1 => format!("{}.{}", self.rng.below(20), self.rng.below(100)),
                2 if self.closure => "#".to_string(),
                3 if self.closure => "#.x".to_string(),
                _ => self
                    .rng
                    .pick(&["a", "b", "c", "n", "o.x", "o.y.z"])
                    .to_string(),
            };
        }
        let d = depth - 1;
        match self.rng.below(9) {
            0..=3 => {
                let op = self.rng.pick(&["+", "-", "*", "/", "%"]);
                format!("({} {op} {})", self.num(d), self.num(d))
            }
            4 => format!("({} ? {} : {})", self.bool(d), self.num(d), self.num(d)),
            5 => format!(
                "{}({})",
                self.rng.pick(&["abs", "round", "floor", "ceil"]),
                self.num(d)
            ),
            6 if !self.closure => {
                self.closure = true;
                let body = self.num(d);
                self.closure = false;
                let list = self.rng.pick(&["items", "list"]);
                format!(
                    "{}(map({list}, {body}))",
                    self.rng.pick(&["sum", "max", "min", "avg"])
                )
            }
            7 => format!("({} ?? {})", self.num(d), self.num(d)),
            _ => format!("-{}", self.num(d)),
        }
    }

    fn bool(&mut self, depth: usize) -> String {
        if depth == 0 || self.rng.chance(20) {
            return match self.rng.below(3) {
                0 => "flag".to_string(),
                1 => self.rng.pick(&["true", "false"]).to_string(),
                _ => format!("s == '{}'", self.rng.pick(&["gold", "x", "abc"])),
            };
        }
        let d = depth - 1;
        match self.rng.below(9) {
            0..=2 => {
                let op = self.rng.pick(&["<", "<=", ">", ">=", "==", "!="]);
                format!("{} {op} {}", self.num(d), self.num(d))
            }
            3 => format!("({} and {})", self.bool(d), self.bool(d)),
            4 => format!("({} or {})", self.bool(d), self.bool(d)),
            5 => format!("not {}", self.bool(d)),
            6 if !self.closure => {
                self.closure = true;
                let body = self.bool(d);
                self.closure = false;
                let kind = self.rng.pick(&["some", "all", "none", "one"]);
                format!("{kind}(items, {body})")
            }
            7 => format!(
                "{} in [{}..{}]",
                self.num(d),
                self.rng.below(10),
                10 + self.rng.below(20)
            ),
            _ => format!("s {} ['gold', 'x']", self.rng.pick(&["in", "not in"])),
        }
    }

    fn string(&mut self, depth: usize) -> String {
        match self.rng.below(4) {
            0 => format!("`${{{}}}:${{s}}`", self.num(depth)),
            1 => format!("({} ? s : t)", self.bool(depth)),
            2 => "upper(s ?? 'z')".to_string(),
            _ => "s + '-' + t".to_string(),
        }
    }

    fn input(rng: &mut Rng) -> Value {
        let mut o = serde_json::Map::new();
        let noise = |rng: &mut Rng, v: Value| match rng.below(100) {
            0..=2 => Value::Null,
            3 => json!("oops"),
            _ => v,
        };
        for k in ["a", "b", "c", "n"] {
            let v = json!(format!("{}.{}", rng.below(40) as i64 - 10, rng.below(10))
                .parse::<f64>()
                .unwrap_or(0.0));
            let v = noise(rng, v);
            if !rng.chance(3) {
                o.insert(k.into(), v);
            }
        }
        let s = json!(["gold", "x", "abc", "silver"][rng.below(4)]);
        o.insert("s".into(), noise(rng, s));
        o.insert("t".into(), json!(["t1", "t2"][rng.below(2)]));
        let flag = json!(rng.chance(50));
        o.insert("flag".into(), noise(rng, flag));
        let items: Vec<Value> = (0..rng.below(7)).map(|_| json!(rng.below(20))).collect();
        o.insert("items".into(), Value::Array(items));
        let list: Vec<Value> = (0..rng.below(5))
            .map(|_| json!({"x": rng.below(10)}))
            .collect();
        o.insert("list".into(), Value::Array(list));
        o.insert(
            "o".into(),
            json!({"x": rng.below(10), "y": {"z": rng.below(10)}}),
        );
        Value::Object(o)
    }
}

#[test]
fn lane_fuzz_typed_matches_stack_vm() {
    let iterations: u64 = std::env::var("LANE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let seed: u64 = std::env::var("LANE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x7EED);
    let mut d = Differential::new(ExpressionKind::Standard);
    for i in 0..iterations {
        let mut rng = Rng(seed
            .wrapping_mul(0x9E3779B97F4A7C15)
            .wrapping_add(i.wrapping_mul(1442695040888963407).wrapping_add(7)));
        let mut g = Typed {
            rng: &mut rng,
            closure: false,
        };
        let expression = match g.rng.below(3) {
            0 => g.num(4),
            1 => g.bool(4),
            _ => g.string(3),
        };
        let inputs: Vec<Value> = (0..64).map(|_| Typed::input(&mut rng)).collect();
        let before = d.failures.len();
        d.check(&expression, &inputs);
        d.columns(&expression, &inputs);
        if d.failures.len() > before {
            eprintln!("typed fuzz case {i} (seed {seed}) failed: {expression}");
        }
    }
    d.assert();
}

#[test]
fn lane_cell_sets_match_unary_evaluation() {
    let iterations: u64 = std::env::var("LANE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let mut runner = LaneRunner::new();
    let mut compared = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for i in 0..iterations {
        let mut rng = Rng(0xCE11u64.wrapping_mul(i.wrapping_add(1)).wrapping_add(17));
        let rules = [3usize, 40, 200][rng.below(3)];
        let cells: Vec<String> = (0..rules)
            .map(|_| match rng.below(12) {
                0 | 1 => String::new(),
                2 => format!("'{}'", ["GB", "US", "DE", "x"][rng.below(4)]),
                3 => format!(
                    "'{}', '{}'",
                    ["GB", "US"][rng.below(2)],
                    ["DE", "FR"][rng.below(2)]
                ),
                4 => format!("{}", rng.below(10)),
                5 => format!(
                    "{} {}",
                    ["<", "<=", ">", ">="][rng.below(4)],
                    rng.below(20) as i64 - 5
                ),
                6 => format!("[{}..{}]", rng.below(10), 10 + rng.below(10)),
                7 => format!("({}..{}]", rng.below(10), 10 + rng.below(10)),
                8 => "$ > limit".to_string(),
                9 => "startsWith($, 'G')".to_string(),
                10 => "true, false".to_string(),
                _ => format!("> {}.5", rng.below(9)),
            })
            .collect();
        let refs: Vec<Option<&str>> = cells.iter().map(|c| Some(c.as_str())).collect();
        let set = zen_expression::lane::CellSet::compile(&refs).expect("compile cells");
        let values: Vec<Variable> = (0..70)
            .map(|_| match rng.below(8) {
                0 => json!(["GB", "US", "DE", "FR", "x"][rng.below(5)]).into(),
                1 => json!(rng.below(25) as i64 - 5).into(),
                2 => json!(format!("{}.{}", rng.below(20), rng.below(10))
                    .parse::<f64>()
                    .unwrap_or(0.0))
                .into(),
                3 => json!(rng.chance(50)).into(),
                4 => Variable::Null,
                5 => json!([1, 2]).into(),
                6 => json!(rng.below(25)).into(),
                _ => json!("Gx").into(),
            })
            .collect();
        let envs: Vec<Scope> = (0..values.len())
            .map(|_| Scope::new(json!({"limit": rng.below(15)}).into()))
            .collect();
        let mut out = Vec::new();
        set.evaluate(&mut runner, &values, &envs, &mut out);
        let words = set.words();
        for (row, value) in values.iter().enumerate() {
            let mut isolate = Isolate::new();
            for (rule, cell) in cells.iter().enumerate() {
                let expected = match cell.trim() {
                    "" => true,
                    c => {
                        isolate.set_environment(envs[row].base().clone());
                        let _ = isolate.set_reference_value(value.clone());
                        isolate.run_unary(c).unwrap_or(false)
                    }
                };
                let got = out[row * words + rule / 64] >> (rule % 64) & 1 == 1;
                compared += 1;
                if got != expected {
                    failures.push(format!(
                        "cell {cell:?} value {value} got {got} expected {expected}"
                    ));
                }
            }
        }
    }
    eprintln!("cell sets compared {compared}");
    assert!(
        failures.is_empty(),
        "{} of {compared} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

type ManyResult = Result<Vec<Value>, (usize, String)>;

#[test]
fn lane_many_matches_expression_node_semantics() {
    let iterations: u64 = std::env::var("LANE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let mut runner = LaneRunner::new();
    let mut compared = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for i in 0..iterations {
        let mut rng = Rng(0xA11u64.wrapping_mul(i.wrapping_add(3)).wrapping_add(99));
        let count = 2 + rng.below(4);
        let mut entries: Vec<(String, String)> = Vec::new();
        for k in 0..count {
            let source = match (k, rng.below(7)) {
                (k, 5 | 6) if k > 0 => {
                    let r = rng.below(k);
                    match rng.below(14) {
                        0 => format!("$.k{r} < 5"),
                        1 => format!("$.k{r} >= 2 ? 'hi' : 'lo'"),
                        2 => format!("$.k{r} == 'a'"),
                        3 => format!("$.k{r} == 1"),
                        4 => format!("$.k{r} != null"),
                        5 => format!("$.k{r} in [1, 2, 'a']"),
                        6 => format!("$.k{r} not in [true, 3]"),
                        7 => format!("$.k{r} and a"),
                        8 => format!("not $.k{r}"),
                        9 => format!("$.k{r}.inner == 2"),
                        10 => format!("$.k{r}.inner > 1"),
                        11 => format!("len($.k{r} ?? [])"),
                        12 => format!("5 > $.k{r}"),
                        _ => format!("$.k{r} * 2 >= b"),
                    }
                }
                (0, _) | (_, 0 | 1 | 5 | 6) => {
                    let typed = rng.chance(50);
                    let mut g = Typed {
                        rng: &mut rng,
                        closure: false,
                    };
                    match typed {
                        true => g.num(3),
                        false => g.bool(2),
                    }
                }
                (_, 2) => format!("$.k{} + 1", rng.below(k)),
                (_, 3) => format!("$.k{} ?? a", rng.below(k)),
                _ => Generator {
                    rng: &mut rng,
                    closures: 0,
                    aliases: Vec::new(),
                }
                .expr(2),
            };
            let key = if rng.chance(15) {
                format!("k{k}.inner")
            } else {
                format!("k{k}")
            };
            entries.push((key, source));
        }
        let inputs: Vec<Value> = (0..70).map(|_| Typed::input(&mut rng)).collect();
        let mask: Vec<bool> = (0..inputs.len()).map(|_| !rng.chance(10)).collect();
        let refs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(k, s)| (k.as_str(), s.as_str()))
            .collect();
        let program = match LaneProgram::compile_many(&refs, true) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let scopes: Vec<Scope> = inputs
            .iter()
            .map(|v| Scope::new(v.clone().into()))
            .collect();
        let program = match rng.chance(50) {
            true => program.specialize(&scopes).expect("specialize"),
            false => program,
        };
        let mut got: Vec<Option<ManyResult>> = vec![None; inputs.len()];
        runner.evaluate_many(&program, &scopes, Some(&mask), |row, r| {
            got[row] = r.map(|r| {
                r.map(|v| v.into_iter().map(|x| x.to_value()).collect())
                    .map_err(|(s, e)| (s, e.to_string()))
            });
        });
        for (row, input) in inputs.iter().enumerate() {
            compared += 1;
            if !mask[row] {
                if got[row].is_some() {
                    failures.push(format!("masked row {row} produced a result"));
                }
                continue;
            }
            let mut isolate = Isolate::with_environment(input.clone().into());
            let mut outputs = Vec::new();
            let mut expected: Result<Vec<Value>, (usize, String)> = Ok(Vec::new());
            for (index, (key, source)) in entries.iter().enumerate() {
                match isolate.run_standard(source) {
                    Ok(v) => {
                        outputs.push(v.to_value());
                        isolate.insert_dollar(key, v);
                    }
                    Err(e) => {
                        expected = Err((index, e.to_string()));
                        break;
                    }
                }
            }
            if expected.is_ok() {
                expected = Ok(outputs);
            }
            if got[row].as_ref() != Some(&expected) {
                failures.push(format!(
                    "{entries:?} | input {input} | lane {:?} vs stack {expected:?}",
                    got[row]
                ));
            }
        }
    }
    eprintln!("many compared {compared}");
    assert!(
        failures.is_empty(),
        "{} of {compared} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn lane_builtins_match_stack_vm() {
    let mut d = Differential::new(ExpressionKind::Standard);
    let inputs = vec![
        json!({"a": 12.5, "s": "Hello World", "b": [3, 1, 2], "t": ["x", "yy", "x"], "o": {"k": 1, "z": [1]}, "e": "a1b2", "text": "Hello World"}),
        json!({"a": -3, "s": "  42 ", "b": [], "t": [], "o": {}, "e": "(a)(b)", "text": "  42 "}),
        json!({"a": 0, "s": "1e3", "b": [1.5, 2.5, 1.5], "t": ["b", "a"], "o": {"a": {"x": 1}, "b": [2]}, "e": "[", "text": "ÄbC ünï ẞ"}),
        json!({"a": null, "s": null, "b": null, "t": null, "o": null, "e": null, "text": "MiXeD"}),
        json!({"a": "7", "s": 5, "b": [1, "x"], "t": [true], "o": [1], "e": 3, "text": ""}),
        json!({"a": 1000000, "s": "true", "b": [[1, 2], [3]], "t": [{"a": 1}, {"b": 2}], "o": {"k": "v"}, "e": ".*", "text": "true"}),
    ];
    let pool = [
        "a",
        "s",
        "b",
        "t",
        "o",
        "e",
        "text",
        "null",
        "true",
        "false",
        "0",
        "2",
        "-1.5",
        "3.14159",
        "'abc'",
        "' x '",
        "'12.50'",
        "'1e3'",
        "''",
        "[1, 2, 3]",
        "['a', 'b']",
        "[]",
        "[1, 'a', null]",
        "[[1], [2, 3]]",
        "[{'a': 1}, {'b': 2}]",
        "{'a': 1}",
        "d('2025-01-02')",
        "[d('2025-01-02'), d('2024-01-01')]",
        "'^[a-z]+$'",
        "'(l+)'",
        "'Europe/Berlin'",
        "'Mars/Olympus'",
    ];
    let functions = [
        "len",
        "contains",
        "upper",
        "lower",
        "trim",
        "startsWith",
        "endsWith",
        "matches",
        "extract",
        "fuzzyMatch",
        "split",
        "abs",
        "sum",
        "avg",
        "min",
        "max",
        "median",
        "mode",
        "floor",
        "ceil",
        "round",
        "trunc",
        "flatten",
        "merge",
        "mergeDeep",
        "keys",
        "values",
        "isNumeric",
        "string",
        "number",
        "bool",
        "type",
        "d",
    ];
    let mut checked = 0usize;
    for function in functions {
        for x in pool {
            let one = format!("{function}({x})");
            d.check(&one, &inputs);
            d.columns(&one, &inputs);
            checked += 1;
            for y in pool {
                let two = format!("{function}({x}, {y})");
                d.check(&two, &inputs);
                d.columns(&two, &inputs);
                checked += 1;
            }
        }
    }
    eprintln!("builtin calls {checked}");
    d.assert();
}

#[test]
fn lane_date_builtins_match_stack_vm() {
    let mut d = Differential::new(ExpressionKind::Standard);
    let inputs = vec![
        json!({"a": 1700000000, "s": "2025-03-15 10:30:00", "t": "2024-02-29", "u": "day"}),
        json!({"a": -86400, "s": "2025-03-15T10:30:00Z", "t": "bogus", "u": "month"}),
        json!({"a": null, "s": null, "t": "2023-12-31", "u": null}),
        json!({"a": "5", "s": 12, "t": "10:30:00", "u": 7}),
    ];
    let pool = [
        "a",
        "s",
        "t",
        "u",
        "null",
        "0",
        "1",
        "-2",
        "1.5",
        "1700000000",
        "'2025-01-02'",
        "'2025-01-02 03:04:05'",
        "'2025-01-02T03:04:05Z'",
        "'10:30'",
        "'10:30:15'",
        "'bogus'",
        "'1d'",
        "'2h 30m'",
        "'day'",
        "'week'",
        "'month'",
        "'year'",
        "'hour'",
        "'quarter'",
        "'minute'",
        "'second'",
        "'decade'",
        "'%Y-%m-%d %H:%M'",
        "'Europe/Berlin'",
        "'UTC'",
        "'Nope/Zone'",
        "d('2024-12-31')",
        "true",
        "[1]",
    ];
    let legacy = [
        "date",
        "time",
        "duration",
        "year",
        "dayOfWeek",
        "dayOfMonth",
        "dayOfYear",
        "weekOfYear",
        "monthOfYear",
        "monthString",
        "dateString",
        "weekdayString",
        "startOf",
        "endOf",
    ];
    let methods = [
        "add",
        "sub",
        "set",
        "format",
        "startOf",
        "endOf",
        "diff",
        "tz",
        "isSame",
        "isBefore",
        "isAfter",
        "isSameOrBefore",
        "isSameOrAfter",
        "second",
        "minute",
        "hour",
        "day",
        "dayOfYear",
        "week",
        "weekday",
        "month",
        "quarter",
        "year",
        "timestamp",
        "offsetName",
        "isValid",
        "isYesterday",
        "isToday",
        "isTomorrow",
        "isLeapYear",
    ];
    let subjects = [
        "d('2025-03-15T10:30:00Z')",
        "d('2024-02-29')",
        "d(s)",
        "d('bogus')",
        "d(a)",
        "d(t, 'Europe/Berlin')",
    ];
    let mut checked = 0usize;
    let mut run = |expression: String, d: &mut Differential| {
        d.check(&expression, &inputs);
        d.columns(&expression, &inputs);
        checked += 1;
    };
    for function in legacy {
        for x in pool {
            run(format!("{function}({x})"), &mut d);
            for y in pool {
                run(format!("{function}({x}, {y})"), &mut d);
            }
        }
    }
    for method in methods {
        for subject in subjects {
            run(format!("{subject}.{method}()"), &mut d);
            for x in pool {
                run(format!("{subject}.{method}({x})"), &mut d);
                for y in ["'day'", "1", "'bogus'", "a", "null"] {
                    run(format!("{subject}.{method}({x}, {y})"), &mut d);
                }
            }
        }
    }
    let flows = [
        "d(s).add(1, 'day').year()",
        "d(s).startOf('month').add(2, 'day').isAfter(d(t))",
        "d(s) > d(t)",
        "d(s) == d(t)",
        "d(s) == d(s).add(0, 'day')",
        "a > 0 ? d(s) : d(t)",
        "a > 0 ? d(s).year() : d(t)",
        "[d(s), d(t).add(1, 'day')]",
        "max([d(s), d(t)])",
        "d(s) ?? d(t)",
        "string(d(s))",
        "type(d(s).add(1, 'day'))",
        "bool(d(s))",
        "{x: d(s), y: d(t).tz('UTC')}",
        "map([s, t], d(#).year())",
        "map([s, t], d(#).add(1, 'day'))",
        "filter([s, t], d(#).isValid())",
        "d(d(s).add(1, 'day')).diff(d(s), 'hour')",
        "d(s).diff(d(t).startOf('year'))",
        "d(s).add(1, 'day').format('%Y')",
        "len(d(s))",
        "d(s) + 1",
        "d(s) in [d(t), d(s)]",
        "d(s).isBefore(d(t)) and d(t).isValid()",
        "not d(s).isLeapYear()",
    ];
    for flow in flows {
        run(flow.to_string(), &mut d);
    }
    eprintln!("date calls {checked}");
    d.assert();
}

#[test]
fn lane_wide_blocks_match_stack_vm() {
    let mut rng = Rng(0xA5A5_5A5A_1234_5678);
    let words = ["gold", "silver", "Bronze", "  pad ", "ÄbC", ""];
    let inputs: Vec<Value> = (0..2500)
        .map(|i| {
            let n = rng.below(40) as i64 - 20;
            let items: Vec<i64> = (0..rng.below(9))
                .map(|_| rng.below(12) as i64 - 3)
                .collect();
            let a = match rng.below(10) {
                0 => json!(null),
                1 => json!("x"),
                2 => json!(n as f64 / 4.0),
                _ => json!(n),
            };
            json!({
                "a": a,
                "b": rng.below(100) as i64 - 50,
                "s": words[rng.below(words.len())],
                "flag": rng.chance(50),
                "items": items,
                "d": format!("2024-0{}-1{} 10:00:00", 1 + rng.below(9), rng.below(10)),
                "row": i,
                "c": format!("{}{}.{}", ["", "-"][rng.below(2)], rng.below(12), 1 + rng.below(9)).parse::<f64>().unwrap_or(0.5),
                "e": format!("{}{}.{}{}", ["", "-"][rng.below(2)], rng.below(3), rng.below(10), 1 + rng.below(9)).parse::<f64>().unwrap_or(0.25),
            })
        })
        .collect();
    let expressions = [
        "a",
        "a + b",
        "a * 1.5 + b",
        "a / 3",
        "b > 0 ? a : b",
        "a > 3 and b < 10",
        "flag or a > 0",
        "a ?? b",
        "a in [1, 5, 9]",
        "b in [-10..10)",
        "s == 'gold'",
        "lower(s) + '-' + upper(s)",
        "trim(s)",
        "len(s)",
        "`${s}:${b}`",
        "round(b * 1.07, 1)",
        "abs(b) - 25",
        "count(items, # > 2)",
        "map(items, # * 2)",
        "filter(items, # > 0)",
        "some(items, # == 1)",
        "all(items, # > -2)",
        "sum(items)",
        "max(items)",
        "d(d).year()",
        "d(d).isBefore(d('2024-05-01'))",
        "{x: a, y: s, z: [b, b * 2]}",
        "row % 7 == 0 ? a + 'x' : b",
        "string(b)",
        "number(string(b)) == b",
        "max([a, b, 1.5])",
        "min([b, b * 1.00])",
        "a ?? 'none'",
        "missing ?? 'none'",
        "missing ?? 0",
        "[none(items, contains(s, 'o')), map(items, len(s))]",
        "[map(items, upper(s)), map(items, lower(s)), count(items, s == 'gold')]",
        "[map(items, lower(s)), map(items, lower(d))]",
        "b > 3.25",
        "b >= 3.25",
        "b < -3.25",
        "b <= -3.25",
        "b == 3.25",
        "b == 3.0",
        "b != 3.25",
        "3.25 < b",
        "-2.5 >= b",
        "b > 3",
        "c > 3",
        "c >= 4.25",
        "c < 3.25",
        "c == 3.5",
        "c == 3.50",
        "c <= -2.75",
        "c > 3.55",
        "4 > c",
        "c != -1.5",
        "e > 1.005",
        "e <= -0.125",
        "e == 0.25",
        "e < 0",
        "e >= -1",
        "c > 99999999999999999.5",
        "-a",
        "-b",
        "-c",
        "-e * 2",
        "-(b - b)",
        "string(-(b - b))",
        "string(-(c - c))",
        "-(-b)",
        "b in [-10..0]",
        "b in (-2.5..2.5]",
        "c in [-1.5..1.5)",
        "e not in [-0.5..0.5]",
        "b not in [-3..3]",
        "a in [-10..-1]",
        "{x: filter(items, len(d) > len(s)), y: map(items, trim(s)), z: some(items, startsWith(d, '2024-01'))}",
    ];
    let mut d = Differential::new(ExpressionKind::Standard);
    for expression in expressions {
        d.check(expression, &inputs);
        d.columns(expression, &inputs);
    }
    d.assert();
}

struct Columnar<'r> {
    rng: &'r mut Rng,
    closure: Option<&'static str>,
}

impl Columnar<'_> {
    const ZONES: [&'static str; 7] = [
        "UTC",
        "Etc/UTC",
        "Europe/Berlin",
        "America/New_York",
        "Asia/Kolkata",
        "Etc/GMT+5",
        "Australia/Lord_Howe",
    ];
    const UNITS: [&'static str; 9] = [
        "second", "minute", "hour", "day", "week", "month", "quarter", "year", "d",
    ];
    const WORDS: [&'static str; 10] = [
        "gold",
        "Silver",
        "  pad ",
        "ÄbC",
        "ünï ẞ",
        "",
        "a-b-c",
        "GOLD",
        "x",
        "Hello World",
    ];

    fn decimal(rng: &mut Rng) -> String {
        let sign = if rng.chance(30) { "-" } else { "" };
        match rng.below(10) {
            0 => format!("{sign}{}", rng.below(1000)),
            1 => format!("{sign}{}.{}", rng.below(100), rng.below(10)),
            2 => format!("{sign}{}.{:02}", rng.below(100), rng.below(100)),
            3 => format!("{sign}0.{:06}", rng.below(1_000_000)),
            4 => format!("{sign}{}.{}0", rng.below(50), rng.below(10)),
            5 => format!("{sign}92233720368547758{:02}", rng.below(8)),
            6 => format!(
                "{sign}{}.{:09}",
                rng.below(1_000_000_000),
                rng.below(1_000_000_000)
            ),
            7 => format!(
                "{sign}0.{}",
                ["5", "25", "125", "1", "10", "100"][rng.below(6)]
            ),
            8 => format!("{sign}{}", rng.below(5)),
            _ => format!("{sign}{}.5", rng.below(20)),
        }
    }

    fn num(&mut self, depth: usize) -> String {
        if depth == 0 || self.rng.chance(25) {
            return match self.rng.below(9) {
                0 => Self::decimal(self.rng),
                1 | 2 if self.closure.is_some() => {
                    self.closure.map_or("#".to_string(), str::to_string)
                }
                3 => self.rng.pick(&["i", "j"]).to_string(),
                4 => "big".to_string(),
                5 => "mixed".to_string(),
                _ => self.rng.pick(&["a", "b", "c"]).to_string(),
            };
        }
        let d = depth - 1;
        match self.rng.below(16) {
            0..=4 => {
                let op = self.rng.pick(&["+", "-", "*", "*", "/", "%", "+", "-"]);
                format!("({} {op} {})", self.num(d), self.num(d))
            }
            5 => format!("({} ? {} : {})", self.bool(d), self.num(d), self.num(d)),
            6 => format!(
                "{}({})",
                self.rng.pick(&["abs", "round", "floor", "ceil", "trunc"]),
                self.num(d)
            ),
            7 => format!(
                "{}({}, {})",
                self.rng.pick(&["round", "trunc"]),
                self.num(d),
                self.rng.below(4)
            ),
            8 if self.closure.is_none() => {
                let list = self.list(d);
                format!(
                    "{}({list})",
                    self.rng
                        .pick(&["sum", "max", "min", "avg", "sum", "median"])
                )
            }
            9 => format!("({} ?? {})", self.num(d), self.num(d)),
            10 => format!(
                "{}([{}, {}, {}])",
                self.rng.pick(&["max", "min"]),
                self.num(d),
                self.num(d),
                self.num(d)
            ),
            11 if self.closure.is_none() => format!("len({})", self.list(d)),
            12 if self.closure.is_none() => {
                let list = self.list(d);
                let body = self.scoped(|g| g.bool(d));
                Self::aliased("count", &list, body)
            }
            13 => format!("len({})", self.string(d)),
            14 => format!(
                "{}.{}()",
                self.date(d),
                self.rng.pick(&[
                    "year",
                    "month",
                    "day",
                    "hour",
                    "weekday",
                    "dayOfYear",
                    "quarter",
                    "week"
                ])
            ),
            _ => format!("-{}", self.num(d)),
        }
    }

    fn scoped(&mut self, body: impl FnOnce(&mut Self) -> String) -> String {
        let alias = self.rng.chance(20).then_some("x");
        self.closure = Some(alias.unwrap_or("#"));
        let out = body(self);
        self.closure = None;
        match alias {
            Some(a) => format!("{a}, {out}"),
            None => out,
        }
    }

    fn list(&mut self, depth: usize) -> String {
        match self.rng.below(9) {
            0 | 1 => self.rng.pick(&["ints", "decs", "items"]).to_string(),
            2 if depth > 0 && self.closure.is_none() => {
                let list = self.rng.pick(&["ints", "decs", "items"]);
                let body = self.scoped(|g| g.num(depth - 1));
                Self::aliased("map", list, body)
            }
            3 if depth > 0 && self.closure.is_none() => {
                let list = self.rng.pick(&["ints", "decs", "items"]);
                let body = self.scoped(|g| g.bool(depth - 1));
                Self::aliased("filter", list, body)
            }
            4 => format!("[{}, {}]", Self::decimal(self.rng), Self::decimal(self.rng)),
            5 => "[]".to_string(),
            6 => format!(
                "{}[{}:{}]",
                self.rng.pick(&["ints", "decs"]),
                self.rng.below(3),
                1 + self.rng.below(4)
            ),
            _ => self.rng.pick(&["ints", "decs"]).to_string(),
        }
    }

    fn aliased(kind: &str, list: &str, body: String) -> String {
        match body.strip_prefix("x, ") {
            Some(rest) => format!("{kind}({list} as x, {rest})"),
            None => format!("{kind}({list}, {body})"),
        }
    }

    fn strings(&mut self, depth: usize) -> String {
        match self.rng.below(4) {
            0 if depth > 0 && self.closure.is_none() => {
                let body = self.scoped(|g| g.string(depth - 1));
                Self::aliased("map", "tags", body)
            }
            1 if depth > 0 && self.closure.is_none() => {
                let body = self.scoped(|g| g.bool(depth - 1));
                Self::aliased("filter", "tags", body)
            }
            _ => "tags".to_string(),
        }
    }

    fn string(&mut self, depth: usize) -> String {
        if depth == 0 || self.rng.chance(25) {
            return match self.rng.below(7) {
                0 => format!("'{}'", Self::WORDS[self.rng.below(Self::WORDS.len())]),
                1 if self.closure.is_some() => self.closure.map_or("#".to_string(), str::to_string),
                2 => "tex".to_string(),
                3 => "st".to_string(),
                _ => "s".to_string(),
            };
        }
        let d = depth - 1;
        match self.rng.below(9) {
            0 => format!(
                "{}({})",
                self.rng.pick(&["lower", "upper", "trim"]),
                self.string(d)
            ),
            1 => format!("{} + {}", self.string(d), self.string(d)),
            2 => format!("`${{{}}}|${{{}}}`", self.string(d), self.num(d)),
            3 => format!(
                "({} ? {} : {})",
                self.bool(d),
                self.string(d),
                self.string(d)
            ),
            4 => format!("string({})", self.num(d)),
            5 => format!("({} ?? {})", self.string(d), self.string(d)),
            6 => format!(
                "{}.format('{}')",
                self.date(d),
                self.rng
                    .pick(&["%Y-%m-%d", "%H:%M", "%Y-%m-%dT%H:%M:%S%z", "%A"])
            ),
            7 => format!(
                "{}[{}:{}]",
                self.string(d),
                self.rng.below(3),
                2 + self.rng.below(4)
            ),
            _ => format!("string({})", self.date(d)),
        }
    }

    fn date(&mut self, depth: usize) -> String {
        let base = match self.rng.below(5) {
            0 => format!("d({})", self.rng.pick(&["d1", "d2", "dt"])),
            1 | 2 => format!(
                "d({}, '{}')",
                self.rng.pick(&["d1", "d2", "dt"]),
                Self::ZONES[self.rng.below(Self::ZONES.len())]
            ),
            3 => format!(
                "d('{}')",
                self.rng.pick(&[
                    "2024-03-31 01:30:00",
                    "2024-10-27 02:30:00",
                    "2024-02-29",
                    "2023-11-05T05:30:00Z"
                ])
            ),
            _ => format!(
                "d(d1).tz('{}')",
                Self::ZONES[self.rng.below(Self::ZONES.len())]
            ),
        };
        let mut out = base;
        for _ in 0..self.rng.below(depth.min(3) + 1) {
            let unit = Self::UNITS[self.rng.below(Self::UNITS.len())];
            out = match self.rng.below(5) {
                0 => format!("{out}.add({}, '{unit}')", self.rng.below(40) as i64 - 10),
                1 => format!("{out}.sub({}, '{unit}')", self.rng.below(40)),
                2 => format!("{out}.startOf('{unit}')"),
                3 => format!("{out}.endOf('{unit}')"),
                _ => format!(
                    "{out}.add('{}')",
                    self.rng.pick(&["1d", "2h 30m", "1w", "36h", "90m"])
                ),
            };
        }
        out
    }

    fn bool(&mut self, depth: usize) -> String {
        if depth == 0 || self.rng.chance(20) {
            return match self.rng.below(4) {
                0 => "flag".to_string(),
                1 => self.rng.pick(&["true", "false"]).to_string(),
                2 => format!("{} > {}", self.num(0), self.num(0)),
                _ => format!("s == '{}'", Self::WORDS[self.rng.below(Self::WORDS.len())]),
            };
        }
        let d = depth - 1;
        match self.rng.below(14) {
            0..=2 => {
                let op = self.rng.pick(&["<", "<=", ">", ">=", "==", "!="]);
                format!("{} {op} {}", self.num(d), self.num(d))
            }
            3 => format!("({} and {})", self.bool(d), self.bool(d)),
            4 => format!("({} or {})", self.bool(d), self.bool(d)),
            5 => format!("not {}", self.bool(d)),
            6 if self.closure.is_none() => {
                let list = self.list(d);
                let body = self.scoped(|g| g.bool(d));
                Self::aliased(self.rng.pick(&["some", "all", "none", "one"]), &list, body)
            }
            7 => format!(
                "{} in [{}..{}]",
                self.num(d),
                self.rng.below(10),
                10 + self.rng.below(20)
            ),
            8 => format!(
                "{}({}, '{}')",
                self.rng.pick(&["contains", "startsWith", "endsWith"]),
                self.string(d),
                self.rng.pick(&["g", "G", "ü", "a-", " ", "old", ""])
            ),
            9 => format!(
                "{}.{}({})",
                self.date(d),
                self.rng
                    .pick(&["isBefore", "isAfter", "isSame", "isSameOrBefore"]),
                self.date(d)
            ),
            10 => format!("{} == {}", self.date(d), self.date(d)),
            11 => format!(
                "{} {} [{}, {}]",
                self.string(d),
                self.rng.pick(&["in", "not in"]),
                self.string(0),
                self.string(0)
            ),
            12 => format!("{} == {}", self.string(d), self.string(d)),
            _ => format!(
                "{} {} ['gold', 'x']",
                self.string(d),
                self.rng.pick(&["in", "not in"])
            ),
        }
    }

    fn top(&mut self) -> String {
        match self.rng.below(12) {
            0..=2 => self.num(4),
            3 | 4 => self.bool(4),
            5 => self.string(3),
            6 => self.date(3),
            7 => self.list(3),
            8 => self.strings(3),
            9 => format!(
                "{{x: {}, y: {}, z: {}}}",
                self.num(2),
                self.list(2),
                self.string(2)
            ),
            10 => format!("[{}, {}]", self.num(3), self.num(3)),
            _ => format!(
                "{}.diff({}, '{}')",
                self.date(2),
                self.date(2),
                Self::UNITS[self.rng.below(Self::UNITS.len())]
            ),
        }
    }

    fn day(rng: &mut Rng) -> String {
        let (y, m, dd) = match rng.below(6) {
            0 => (2024, 3, 31),
            1 => (2024, 10, 27),
            2 => (2023, 11, 5),
            3 => (2024, 2, 29),
            _ => (2000 + rng.below(40), 1 + rng.below(12), 1 + rng.below(28)),
        };
        format!("{y:04}-{m:02}-{dd:02}")
    }

    fn input(rng: &mut Rng, noisy: bool) -> Value {
        let mut o = serde_json::Map::new();
        let num = |rng: &mut Rng| -> Value {
            serde_json::from_str(&Self::decimal(rng)).unwrap_or(Value::Null)
        };
        for k in ["a", "b", "c"] {
            match rng.below(100) {
                0..=4 => {}
                5..=7 => {
                    o.insert(k.into(), Value::Null);
                }
                8 if noisy => {
                    o.insert(k.into(), json!("oops"));
                }
                _ => {
                    o.insert(k.into(), num(rng));
                }
            }
        }
        for k in ["i", "j"] {
            if !rng.chance(4) {
                o.insert(k.into(), json!(rng.below(2000) as i64 - 1000));
            }
        }
        let big = match rng.below(3) {
            0 => i64::MAX - rng.below(10) as i64,
            1 => i64::MIN + rng.below(10) as i64,
            _ => rng.below(1 << 40) as i64 * 4096,
        };
        o.insert("big".into(), json!(big));
        let mixed = match rng.below(5) {
            0 => json!("12.5"),
            1 => json!(true),
            2 => Value::Null,
            _ => num(rng),
        };
        o.insert("mixed".into(), mixed);
        o.insert("s".into(), json!(Self::WORDS[rng.below(Self::WORDS.len())]));
        o.insert(
            "st".into(),
            json!(Self::WORDS[rng.below(Self::WORDS.len())]),
        );
        if !rng.chance(5) {
            o.insert(
                "tex".into(),
                json!(Self::WORDS[rng.below(Self::WORDS.len())]),
            );
        }
        o.insert("flag".into(), json!(rng.chance(50)));
        let hour = rng.below(24);
        let d1 = match rng.below(12) {
            0 => json!("bogus"),
            1 => Value::Null,
            2 => json!(format!("{}T{hour:02}:30:00+02:00", Self::day(rng))),
            3 => json!(format!("{} {hour:02}:15:00", Self::day(rng))),
            _ => json!(format!("{}T{hour:02}:30:00Z", Self::day(rng))),
        };
        o.insert("d1".into(), d1);
        o.insert("d2".into(), json!(Self::day(rng)));
        o.insert(
            "dt".into(),
            json!(format!(
                "{} {:02}:{:02}:{:02}",
                Self::day(rng),
                rng.below(24),
                rng.below(60),
                rng.below(60)
            )),
        );
        let ints: Vec<Value> = (0..rng.below(9))
            .map(|_| json!(rng.below(40) as i64 - 10))
            .collect();
        o.insert("ints".into(), Value::Array(ints));
        let decs: Vec<Value> = match rng.below(4) {
            0 => {
                let v = num(rng);
                let neg: Value =
                    serde_json::from_str(&format!("-{v}").replace("--", "")).unwrap_or(Value::Null);
                vec![v, neg]
            }
            _ => (0..rng.below(7)).map(|_| num(rng)).collect(),
        };
        o.insert("decs".into(), Value::Array(decs));
        let items: Vec<Value> = (0..rng.below(6))
            .map(|_| match (noisy, rng.below(10)) {
                (true, 0) => json!("z"),
                (true, 1) => Value::Null,
                _ => json!(rng.below(20)),
            })
            .collect();
        o.insert("items".into(), Value::Array(items));
        let tags: Vec<Value> = (0..rng.below(5))
            .map(|_| json!(Self::WORDS[rng.below(Self::WORDS.len())]))
            .collect();
        o.insert("tags".into(), Value::Array(tags));
        Value::Object(o)
    }
}

#[test]
fn lane_fuzz_columnar_matches_stack_vm() {
    let iterations: u64 = std::env::var("LANE_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let seed: u64 = std::env::var("LANE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0xC0105);
    let max_rows: usize = std::env::var("LANE_FUZZ_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1100);
    let mut d = Differential::new(ExpressionKind::Standard);
    for i in 0..iterations {
        let mut rng = Rng(seed
            .wrapping_mul(0xD1B54A32D192ED03)
            .wrapping_add(i.wrapping_mul(0x9E3779B97F4A7C15).wrapping_add(11)));
        let generated = Columnar {
            rng: &mut rng,
            closure: None,
        }
        .top();
        if std::env::var("LANE_FUZZ_CASE").is_ok_and(|c| c != i.to_string()) {
            continue;
        }
        let expression = std::env::var("LANE_FUZZ_EXPR").unwrap_or(generated);
        let rows =
            [1, 3, 63, 64, 65, 130, 1023, 1024, 1025, 2500][rng.below(10)].min(max_rows.max(1));
        let noisy = rng.chance(30);
        let inputs: Vec<Value> = (0..rows)
            .map(|_| Columnar::input(&mut rng, noisy))
            .collect();
        let before = d.failures.len();
        d.check(&expression, &inputs);
        d.columns(&expression, &inputs);
        if std::env::var("LANE_FUZZ_SHOW").is_ok() {
            eprintln!("{rows}\t{expression}");
        }
        if d.failures.len() > before {
            eprintln!("columnar fuzz case {i} (seed {seed}, rows {rows}) failed: {expression}");
        }
    }
    d.assert();
}

struct SpecLane {
    runner: LaneRunner,
    failures: Vec<String>,
    compared: usize,
    unstable: usize,
    kernels: usize,
    compiled: usize,
    rows: usize,
    hits: usize,
}

impl SpecLane {
    fn scope(case: &conformance::SpecCase) -> Scope {
        match &case.input {
            Some(v) => Scope::new(v.depth_clone(64)),
            None => Scope::default(),
        }
    }

    fn normalize(
        kind: &ExpressionKind,
        got: Result<Variable, zen_expression::IsolateError>,
    ) -> Result<Variable, String> {
        match (kind, got) {
            (ExpressionKind::Unary, Ok(v)) => v
                .as_bool()
                .map(Variable::Bool)
                .ok_or_else(|| zen_expression::IsolateError::ValueCastError.to_string()),
            (_, r) => r.map_err(|e| e.to_string()),
        }
    }

    fn same(a: &Result<Variable, String>, b: &Result<Variable, String>) -> bool {
        match (a, b) {
            (Ok(x), Ok(y)) => conformance::Spec::same(x, y) && conformance::Spec::same(y, x),
            (Err(x), Err(y)) => x == y,
            _ => false,
        }
    }

    fn compare(
        &mut self,
        label: &str,
        case: &conformance::SpecCase,
        want: &Result<Variable, String>,
        got: Result<Variable, String>,
    ) {
        self.compared += 1;
        if !Self::same(want, &got) {
            let show = |r: &Result<Variable, String>| match r {
                Ok(v) => conformance::Spec::render(v),
                Err(e) => format!("!error ({e})"),
            };
            self.failures.push(format!(
                "{} | {} | input {} | {label} | lane {} vs stack {}",
                case.location(),
                case.expression,
                case.input_text,
                show(&got),
                show(want)
            ));
        }
    }

    fn guarded<T>(&mut self, f: impl FnOnce(&mut LaneRunner) -> T) -> Option<T> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut self.runner)));
        if result.is_err() {
            self.runner = LaneRunner::new();
        }
        result.ok()
    }

    fn panicked(n: usize) -> Vec<Result<Variable, zen_expression::IsolateError>> {
        (0..n)
            .map(|_| Err(zen_expression::IsolateError::ValueCastError))
            .collect()
    }

    fn group(&mut self, cases: &[&conformance::SpecCase]) -> bool {
        let Some(first) = cases.first() else {
            return true;
        };
        let (kind, expression) = (first.kind.clone(), first.expression.as_str());
        let want: Vec<Result<Variable, String>> = cases.iter().map(|c| c.stack()).collect();
        let again: Vec<Result<Variable, String>> = cases.iter().map(|c| c.stack()).collect();
        if want.iter().zip(&again).any(|(a, b)| !Self::same(a, b)) {
            self.unstable += cases.len();
            return false;
        }
        let program = match LaneProgram::compile(expression, kind.clone()) {
            Ok(p) => p,
            Err(e) => {
                for (case, want) in cases.iter().zip(&want) {
                    self.compare("compile", case, want, Err(e.to_string()));
                }
                return true;
            }
        };
        let scopes = || cases.iter().map(|c| Self::scope(c)).collect::<Vec<_>>();
        let plain = program.without_kernel();
        self.compiled += 1;
        match program.has_kernel() {
            true => self.kernels += 1,
            false => self.failures.push(format!("{expression} | compiled without a kernel")),
        }
        for case in cases.iter() {
            self.rows += 1;
            if self.guarded(|_| program.try_kernel(&Self::scope(case))).flatten().is_some() {
                self.hits += 1;
            }
        }
        for (case, want) in cases.iter().zip(&want) {
            let got = match self.guarded(|r| r.evaluate_one(&plain, &Self::scope(case)).map(conformance::Spec::settle)) {
                Some(got) => Self::normalize(&kind, got),
                None => Err(format!("{}: lane", conformance::Spec::PANIC)),
            };
            let got = match (want, got) {
                (Err(w), Err(g))
                    if w.starts_with(conformance::Spec::PANIC)
                        && g.starts_with(conformance::Spec::PANIC) =>
                {
                    Err(w.clone())
                }
                (_, got) => got,
            };
            self.compare("width 1 without kernel", case, want, got);
        }
        for (case, want) in cases.iter().zip(&want) {
            let got = match self.guarded(|r| r.evaluate_one(&program, &Self::scope(case)).map(conformance::Spec::settle)) {
                Some(got) => Self::normalize(&kind, got),
                None => Err(format!("{}: lane", conformance::Spec::PANIC)),
            };
            let got = match (want, got) {
                (Err(w), Err(g))
                    if w.starts_with(conformance::Spec::PANIC)
                        && g.starts_with(conformance::Spec::PANIC) =>
                {
                    Err(w.clone())
                }
                (_, got) => got,
            };
            self.compare("width 1", case, want, got);
        }
        if want.iter().any(|w| {
            w.as_ref()
                .is_err_and(|e| e.starts_with(conformance::Spec::PANIC))
        }) {
            return false;
        }
        let n = cases.len();
        let batch = self
            .guarded(|r| r.evaluate(&program, &scopes()))
            .unwrap_or_else(|| Self::panicked(n));
        for ((case, want), got) in cases.iter().zip(&want).zip(batch) {
            self.compare("batch", case, want, Self::normalize(&kind, got));
        }
        let batch = self
            .guarded(|r| r.evaluate(&plain, &scopes()))
            .unwrap_or_else(|| Self::panicked(n));
        for ((case, want), got) in cases.iter().zip(&want).zip(batch) {
            self.compare("batch without kernel", case, want, Self::normalize(&kind, got));
        }
        if let Ok(specialized) = program.specialize(&scopes()) {
            for (case, want) in cases.iter().zip(&want) {
                let got = match self.guarded(|r| r.evaluate_one(&specialized, &Self::scope(case)).map(conformance::Spec::settle)) {
                    Some(got) => Self::normalize(&kind, got),
                    None => Err(format!("{}: lane", conformance::Spec::PANIC)),
                };
                self.compare("specialized", case, want, got);
            }
            let batch = self
                .guarded(|r| r.evaluate(&specialized, &scopes()))
                .unwrap_or_else(|| Self::panicked(n));
            for ((case, want), got) in cases.iter().zip(&want).zip(batch) {
                self.compare("specialized batch", case, want, Self::normalize(&kind, got));
            }
        }
        let shape = first.input.as_ref().map_or(Value::Null, |v| v.to_value());
        if let Ok(typed) =
            LaneProgram::compile_typed(expression, kind.clone(), &VariableType::from(&shape))
        {
            let batch = self
                .guarded(|r| r.evaluate(&typed, &scopes()))
                .unwrap_or_else(|| Self::panicked(n));
            for ((case, want), got) in cases.iter().zip(&want).zip(batch) {
                self.compare("typed", case, want, Self::normalize(&kind, got));
            }
        }
        true
    }
}

#[test]
fn lane_matches_spec() {
    conformance::Spec::prepare();
    let (cases, problems) = conformance::Spec::cases();
    assert!(
        problems.is_empty(),
        "malformed spec:\n{}",
        problems.join("\n")
    );
    let mut groups: BTreeMap<(bool, String), Vec<&conformance::SpecCase>> = BTreeMap::new();
    for case in &cases {
        let unary = matches!(case.kind, ExpressionKind::Unary);
        groups
            .entry((unary, case.expression.clone()))
            .or_default()
            .push(case);
    }
    let mut lane = SpecLane {
        runner: LaneRunner::new(),
        failures: Vec::new(),
        compared: 0,
        unstable: 0,
        kernels: 0,
        compiled: 0,
        rows: 0,
        hits: 0,
    };
    let mut standard = Differential::new(ExpressionKind::Standard);
    let mut unary = Differential::new(ExpressionKind::Unary);
    conformance::Spec::prepare();
    for ((is_unary, expression), group) in &groups {
        if !lane.group(group) {
            continue;
        }
        let inputs: Vec<Value> = group
            .iter()
            .map(|c| c.input.as_ref().map_or(Value::Null, |v| v.to_value()))
            .collect();
        match is_unary {
            true => unary.columns(expression, &inputs),
            false => standard.columns(expression, &inputs),
        }
    }
    eprintln!(
        "spec cases {} | lane row comparisons {} | skipped {} cases whose stack result is nondeterministic | kernels for {} of {} compiled groups, {}/{} rows settled in the kernel",
        cases.len(),
        lane.compared,
        lane.unstable,
        lane.kernels,
        lane.compiled,
        lane.hits,
        lane.rows
    );
    let shown: Vec<String> = lane.failures.iter().take(60).cloned().collect();
    assert!(
        lane.failures.is_empty(),
        "{} lane/stack differences:\n{}",
        lane.failures.len(),
        shown.join("\n")
    );
    for d in [standard, unary] {
        if d.compared > 0 {
            d.assert();
        }
    }
}

#[test]
fn lane_regression_cloned_programs_keep_object_keys() {
    let src = "{a: {x: 1, y: 2}, b: {p: 3, q: 4}}";
    let scope = Scope::new(Variable::from(json!({})));
    let expected = Isolate::new().run_standard(src).expect("old vm").to_value().to_string();
    for _ in 0..40 {
        let mut runner = LaneRunner::new();
        let first = LaneProgram::standard(src).expect("compile");
        let _ = runner.evaluate_one(&first, &scope);
        let second = first.clone();
        drop(first);
        let mut current = second.clone();
        for _ in 0..50 {
            let got = runner.evaluate_one(&current, &scope).expect("lane").to_value().to_string();
            assert_eq!(got, expected);
            let next = current.clone();
            drop(current);
            current = next;
        }
    }
}

#[test]
fn lane_regression_many_entries_do_not_share_assignments() {
    let cases: [(&[(&str, &str)], Value); 3] = [
        (&[("k0", "x = a; x"), ("k1", "x")], json!({"a": 1})),
        (&[("k0", "a.b = 1; 5"), ("k2", "a")], json!({})),
        (&[("k1", "map(l, (t = #; t))"), ("k2", "t"), ("k3", "$.k1")], json!({"l": [1, 2]})),
    ];
    let mut runner = LaneRunner::new();
    for (entries, input) in cases {
        let mut isolate = Isolate::with_environment(input.clone().into());
        let mut expected = Vec::new();
        for (key, source) in entries.iter() {
            let v = isolate.run_standard(source).expect("old vm");
            expected.push(v.to_value());
            isolate.insert_dollar(key, v);
        }
        let program = LaneProgram::compile_many(entries, true).expect("compile");
        let mut got = None;
        runner.evaluate_many(&program, &[Scope::new(input.clone().into())], None, |_, r| {
            got = r.map(|r| r.map(|v| v.into_iter().map(|x| x.to_value()).collect::<Vec<_>>()).ok())
        });
        assert_eq!(got.flatten(), Some(expected), "{entries:?}");
    }
}

#[test]
fn lane_regression_empty_many_program_does_not_panic() {
    let program = LaneProgram::compile_many(&[], true).expect("compile");
    let mut runner = LaneRunner::new();
    let scope = Scope::new(Variable::from(json!({"a": 1})));
    let _ = runner.evaluate_one(&program, &scope);
    runner.evaluate_many(&program, std::slice::from_ref(&scope), None, |_, _| {});
    let columns = Columns::new(3);
    let mut out = Output::new();
    runner.evaluate_columns_into(&program, &columns, &mut out);
}

#[test]
fn lane_regression_dates_cross_between_engines() {
    std::env::set_var("TZ", "UTC");
    let expressions = ["d(x).year()", "x.year()", "d(x)", "d(x) > d('2024-01-01')", "x.diff(d('2024-03-15 10:00:00'))", "[x, 1]"];
    let mut runner = LaneRunner::new();
    let old_date = Isolate::new().run_standard("d('2024-03-15 10:00:00')").expect("old vm");
    let lane_date = runner
        .evaluate_one(&LaneProgram::standard("d('2024-03-15 10:00:00')").expect("compile"), &Scope::default())
        .expect("lane");
    for date in [old_date, lane_date] {
        let input = Variable::from(json!({"x": null}));
        input.dot_insert("x", date);
        for e in expressions {
            let want = Isolate::with_environment(input.depth_clone(64)).run_standard(e).map(|v| v.to_value()).map_err(|e| e.to_string());
            let program = LaneProgram::standard(e).expect("compile");
            let got = runner.evaluate_one(&program, &Scope::new(input.depth_clone(64))).map(|v| v.to_value()).map_err(|e| e.to_string());
            assert_eq!(got, want, "{e}");
            let again = runner.evaluate_one(&program, &Scope::new(input.depth_clone(64))).expect("lane");
            let back = Variable::from(json!({"y": null}));
            back.dot_insert("y", again);
            let reread = Isolate::with_environment(back).run_standard("y").map(|v| v.to_value()).map_err(|e| e.to_string());
            assert_eq!(reread, want, "old vm re-reading lane output of {e}");
        }
    }
}

struct Rows;

impl Rows {
    fn check(columns: &Columns, expressions: &[&str]) {
        let mut runner = LaneRunner::new();
        for e in expressions {
            let program = LaneProgram::standard(e).expect("compile");
            for program in [program.clone(), program.specialize_columns(columns).expect("specialize")] {
                let mut out = Output::new();
                runner.evaluate_columns_into(&program, columns, &mut out);
                for r in 0..columns.rows {
                    let want = Isolate::with_environment(columns.row(r)).run_standard(e).map(|v| v.to_value()).ok();
                    let got = out.variable(r).and_then(|x| x.ok()).map(|v| v.to_value());
                    assert_eq!(got, want, "{e} row {r}");
                }
            }
        }
    }
}

#[test]
fn lane_regression_list_of_any_aggregates() {
    let child = [Variable::Number(Decimal::ONE), Variable::Number(Decimal::new(25, 1)), Variable::Null, Variable::Number(Decimal::new(-5, 1))];
    let offsets = [0i32, 2, 4];
    let child = Column::new(Values::Any(&child));
    let columns = Columns::new(2).column("l", Column::new(Values::List { offsets: &offsets, child: (&child).into() }));
    Rows::check(&columns, &["sum(l)", "avg(l)", "median(l)", "mode(l)", "max(l)", "len(l)"]);
}

#[test]
fn lane_regression_row_rebuild_does_not_mutate_input() {
    let shared = Variable::from(json!({"k": 0}));
    let objects = [shared.clone(), shared.clone()];
    let ys = [1i64, 2];
    let columns = Columns::new(2)
        .column("o", Column::new(Values::Any(&objects)))
        .column("o.y", Column::new(Values::I64(&ys)));
    Rows::check(&columns, &["o.y", "o.k", "o"]);
    assert_eq!(shared.to_value(), json!({"k": 0}));
}

#[test]
fn lane_regression_rand_is_not_memoized() {
    let keys = vec![0i32; 1000];
    let values = [Decimal::from(1_000_000_000)];
    let dictionary = Column::new(Values::Dec(&values));
    let columns = Columns::new(1000).column("a", Column::new(Values::Dict { keys: &keys, values: (&dictionary).into() }));
    let mut out = Output::new();
    LaneRunner::new().evaluate_columns_into(&LaneProgram::standard("rand(a)").expect("compile"), &columns, &mut out);
    let distinct: std::collections::BTreeSet<String> = (0..1000).filter_map(|r| out.variable(r)).filter_map(|v| v.ok()).map(|v| v.to_value().to_string()).collect();
    assert!(distinct.len() > 100, "{}", distinct.len());
}

#[test]
fn lane_regression_cells_near_decimal_max() {
    let max = "79228162514264337593543950335";
    let cells = [format!("> {max}"), format!("< -{max}"), "> 79228162514264337593543950330".to_string(), "< 79228162514264337593543950000".to_string()];
    let refs: Vec<Option<&str>> = cells.iter().map(|c| Some(c.as_str())).collect();
    let set = zen_expression::lane::CellSet::compile(&refs).expect("compile");
    let values: Vec<Variable> = [json!(5), json!(max.parse::<f64>().unwrap_or(0.0)), json!(-1)].into_iter().map(Variable::from).collect();
    let envs: Vec<Scope> = values.iter().map(|_| Scope::default()).collect();
    let mut out = Vec::new();
    set.evaluate(&mut LaneRunner::new(), &values, &envs, &mut out);
    for (row, value) in values.iter().enumerate() {
        for (rule, cell) in cells.iter().enumerate() {
            let mut isolate = Isolate::new();
            let _ = isolate.set_reference_value(value.clone());
            let expected = isolate.run_unary(cell).unwrap_or(false);
            let got = out[row * set.words() + rule / 64] >> (rule % 64) & 1 == 1;
            assert_eq!(got, expected, "{cell} {value}");
        }
    }
}

#[test]
fn lane_regression_oversized_expression_errors_cleanly() {
    let items: Vec<String> = (0..30_000).map(|i| format!("'s{i}'")).collect();
    let source = format!("len([{}])", items.join(", "));
    assert!(LaneProgram::standard(&source).is_err());
}

#[test]
fn lane_regression_malformed_offsets() {
    let data = "aé".as_bytes();
    let split = [0i32, 1, 2, 3];
    let columns = Columns::new(3).column("s", Column::new(Values::Utf8 { offsets: &split, data }));
    Rows::check(&columns, &["s", "s == null", "s ?? 'n'", "s + 'x'"]);
    let items = [1i64, 2, 3, 4];
    let child = Column::new(Values::I64(&items));
    for offsets in [[3i32, 1, 4], [-2, 1, 4]] {
        let columns = Columns::new(2).column("l", Column::new(Values::List { offsets: &offsets, child: (&child).into() }));
        Rows::check(&columns, &["len(l)", "count(l, true)", "sum(l)", "map(l, # * 2)"]);
    }
}

#[test]
fn lane_regression_duplicate_column_names() {
    let first = [1i64, 2];
    let second = [10i64, 20];
    let columns = Columns::new(2)
        .column("a", Column::new(Values::I64(&first)))
        .column("a", Column::new(Values::I64(&second)));
    Rows::check(&columns, &["a", "a + 1"]);
}

#[test]
fn lane_regression_chained_dollar_matches_isolate() {
    let cases: [(&[(&str, &str)], Value); 14] = [
        (&[("k0", "[1, 2]"), ("k1", "$.k0[1]")], json!({})),
        (&[("k0", "{l: [5]}"), ("k1", "$.k0.l[0] + 1")], json!({})),
        (&[("k0", "items"), ("k1", "$.k0[1]"), ("k2", "{ok: $.k0[0]}")], json!({"items": [10, 20], "$": {"k0": [7, 8]}})),
        (&[("k0", "s"), ("k1", "$.k0[0]")], json!({"s": "gold"})),
        (&[("k0", "1"), ("k1", "a.b = 7; 2"), ("k2", "$.k0")], json!({"a": {"b": 1}})),
        (&[("k0", "1"), ("k1", "$ = 3; 2"), ("k2", "$.k0")], json!({"a": {"b": 1}})),
        (&[("k0", "{p: 1}"), ("k1", "$.k0.p = 5; 2"), ("k2", "$.k0.p")], json!({})),
        (&[("k0", "1"), ("k1", "$root.$.k0"), ("k2", "keys($root.$)")], json!({})),
        (&[("k0", "$ == null"), ("k1", "type($)")], json!({"a": 1})),
        (&[("k0", "keys($root)"), ("k1.x", "1")], json!({"a": 1})),
        (&[("k0", "$")], json!({})),
        (&[("k0", "$.x")], json!({"$": {"x": 5}})),
        (&[("k0", "$.k0 ?? 0"), ("k0", "$.k0 + 1")], json!({"$": {"k0": 7}})),
        (&[("k0.a", "1"), ("k1", "$.k0.a + $root.$.k0.a")], json!({})),
    ];
    let mut runner = LaneRunner::new();
    for (entries, input) in cases {
        let mut isolate = Isolate::with_environment(input.clone().into());
        let mut expected = Vec::new();
        for (key, source) in entries.iter() {
            let v = isolate.run_standard(source).map(|v| v.to_value()).ok();
            expected.push(v.clone());
            isolate.insert_dollar(key, v.map(Variable::from).unwrap_or(Variable::Null));
        }
        let program = LaneProgram::compile_many(entries, true).expect("compile");
        let scope = Scope::new(input.clone().into());
        for program in [program.clone(), program.specialize(std::slice::from_ref(&scope)).expect("specialize")] {
            let mut got = None;
            runner.evaluate_many(&program, std::slice::from_ref(&scope), None, |_, r| {
                got = r.and_then(|r| r.ok()).map(|v| v.into_iter().map(|x| Some(x.to_value())).collect::<Vec<_>>())
            });
            assert_eq!(got, Some(expected.clone()), "{entries:?}");
        }
    }
}

#[test]
fn lane_regression_cells_survive_invalid_neighbours() {
    let cells = [Some("$ > limit"), Some("@"), Some("startsWith($, 'a')"), Some("-"), Some("!= 2")];
    let refs: Vec<Option<&str>> = cells.to_vec();
    let set = zen_expression::lane::CellSet::compile(&refs).expect("compile");
    let values: Vec<Variable> = [json!(10), json!("abc"), json!(2)].into_iter().map(Variable::from).collect();
    let envs: Vec<Scope> = values.iter().map(|_| Scope::new(json!({"limit": 2}).into())).collect();
    let mut out = Vec::new();
    set.evaluate(&mut LaneRunner::new(), &values, &envs, &mut out);
    for (row, value) in values.iter().enumerate() {
        for (rule, cell) in cells.iter().enumerate() {
            let mut isolate = Isolate::with_environment(json!({"limit": 2}).into());
            let _ = isolate.set_reference_value(value.clone());
            let expected = cell.and_then(|c| isolate.run_unary(c).ok()).unwrap_or(false);
            let got = out[row * set.words() + rule / 64] >> (rule % 64) & 1 == 1;
            assert_eq!(got, expected, "{cell:?} {value}");
        }
    }
}

#[test]
fn lane_regression_cells_probe_between_close_bounds() {
    let cells = ["< 7.9228162514264337593543950335", "[7.9228162514264337593543950333..7.9228162514264337593543950335)"];
    let refs: Vec<Option<&str>> = cells.iter().map(|c| Some(*c)).collect();
    let set = zen_expression::lane::CellSet::compile(&refs).expect("compile");
    let values: Vec<Variable> = ["7.9228162514264337593543950334", "7.9228162514264337593543950333", "7.9228162514264337593543950335"]
        .iter()
        .filter_map(|s| s.parse::<Decimal>().ok())
        .map(Variable::Number)
        .collect();
    let envs: Vec<Scope> = values.iter().map(|_| Scope::default()).collect();
    let mut out = Vec::new();
    set.evaluate(&mut LaneRunner::new(), &values, &envs, &mut out);
    for (row, value) in values.iter().enumerate() {
        for (rule, cell) in cells.iter().enumerate() {
            let mut isolate = Isolate::new();
            let _ = isolate.set_reference_value(value.clone());
            let expected = isolate.run_unary(cell).unwrap_or(false);
            let got = out[row * set.words() + rule / 64] >> (rule % 64) & 1 == 1;
            assert_eq!(got, expected, "{cell} {value}");
        }
    }
}

#[test]
fn lane_regression_malformed_offsets_on_fast_paths() {
    let data = "aébc".as_bytes();
    for offsets in [[3i32, 1, 3], [0, 3, 1], [1, i32::MIN, 3]] {
        let columns = Columns::new(2).column("s", Column::new(Values::Utf8 { offsets: &offsets, data }));
        Rows::check(&columns, &["s", "s == 'é'", "s == 'aé'", "upper(s)", "len(s)", "s in ['é', 'aé']", "s == 'é' ? 1 : (s == 'aé' ? 2 : 3)"]);
        let large: Vec<i64> = offsets.iter().map(|o| *o as i64).collect();
        let columns = Columns::new(2).column("s", Column::new(Values::LargeUtf8 { offsets: &large, data }));
        Rows::check(&columns, &["s == 'é'", "s == 'aé'"]);
    }
    let items = [5i64, 6];
    let child = Column::new(Values::I64(&items));
    for offsets in [&[0i32, 1][..], &[0, 1, 4, 4][..]] {
        let columns = Columns::new(3).column("a", Column::new(Values::List { offsets, child: (&child).into() }));
        Rows::check(&columns, &["len(a)", "sum(a)", "filter(a, # != null)", "filter(a, # > 5)", "count(a, # == null)"]);
    }
}

#[test]
fn lane_regression_date_inputs_keep_their_text() {
    std::env::set_var("TZ", "UTC");
    let texts = ["2024-03-15", "2024-03-15T10:30:00+02:00", "2024-03-15 10:30", "20240315"];
    let dates: Vec<Variable> = texts.iter().filter_map(|t| zen_expression::DateValue::from_text(t)).collect();
    let values: Vec<Variable> = dates.iter().cloned().chain([Variable::String("2024-03-15".into()), Variable::Null]).collect();
    let expressions = [
        "x", "string(x)", "x + ''", "`${x}`", "len(x)", "upper(x)", "x[0:3]", "bool(x)", "x == '2024-03-15'",
        "x > '2024-03-01'", "x in ['2024-03-15']", "contains([x], '2024-03-15')", "d(x)", "d(x, 'Europe/Berlin')",
        "x.year()", "x.format(x)", "year(x)", "time(x)", "fuzzyMatch(x, '2024')", "[x, x]", "{v: x}",
    ];
    let scopes: Vec<Scope> = values
        .iter()
        .map(|v| {
            let input = Variable::from(json!({"x": null}));
            input.dot_insert("x", v.clone());
            Scope::new(input)
        })
        .collect();
    let want = |e: &str, s: &Scope| Isolate::with_environment(s.base().clone()).run_standard(e).map(|v| v.to_value()).ok();
    let date_type = VariableType::Object(std::rc::Rc::new(std::cell::RefCell::new(
        [(std::rc::Rc::<str>::from("x"), VariableType::Date)].into_iter().collect(),
    )));
    let mut runner = LaneRunner::new();
    for e in expressions {
        let plain = LaneProgram::standard(e).expect("compile");
        let programs = [
            plain.clone(),
            plain.specialize(&scopes).expect("specialize"),
            LaneProgram::compile_typed(e, ExpressionKind::Standard, &date_type).expect("typed"),
        ];
        for program in &programs {
            let got: Vec<_> = runner.evaluate(program, &scopes).into_iter().map(|r| r.map(|v| v.to_value()).ok()).collect();
            let expected: Vec<_> = scopes.iter().map(|s| want(e, s)).collect();
            assert_eq!(got, expected, "{e}");
        }
        let columns = Columns::new(values.len()).column("x", Column::new(Values::Any(&values)));
        let mut out = Output::new();
        runner.evaluate_columns_into(&plain, &columns, &mut out);
        for (r, s) in scopes.iter().enumerate() {
            assert_eq!(out.variable(r).and_then(|x| x.ok()).map(|v| v.to_value()), want(e, s), "{e} column row {r}");
        }
    }
}

#[test]
fn lane_regression_constant_folding_keeps_runtime_semantics() {
    std::env::set_var("TZ", "UTC");
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-03-20T10:15:30Z");
    let cases = [
        "1 + 2 * 3", "(1 / 0) ?? 5", "1.50 + 1.00", "-0", "0 * -1", "string(1.500)", "d('2024-01-01')",
        "d('2024-01-01', 'Europe/Berlin')", "d('2024-01-01T10:00:00+02:00') > d('2024-01-01')",
        "d('2024-01-01').add(1, 'd').format('%Y-%m-%d')", "d('now').isValid()", "d().year() > 2000",
        "d('2024-01-01').isToday()", "d('UTC').isValid()", "x ? d('2023-10-05').format('%Q') : 1",
        "x > d('2024-01-01')", "len([1, 2, 3]) + x", "upper('abc') + x", "({a: 1}).a + 1", "[1, 2][0]",
        "1 + {}", "len('héllo')", "`a${1 + 1}b`", "5 in [1..10]", "'a' in ['a', 'b']",
    ];
    let mut runner = LaneRunner::new();
    let scope = Scope::new(Variable::from(json!({"x": false})));
    for e in cases {
        let want = Isolate::with_environment(Variable::from(json!({"x": false}))).run_standard(e).map(|v| v.to_value()).ok();
        let program = LaneProgram::standard(e).expect("compile");
        for _ in 0..2 {
            let got = runner.evaluate_one(&program, &scope).map(|v| v.to_value()).ok();
            assert_eq!(got, want, "{e}");
        }
    }
    let rand = LaneProgram::standard("rand(1000000)").expect("compile");
    let values: std::collections::BTreeSet<String> = (0..20)
        .filter_map(|_| runner.evaluate_one(&rand, &scope).ok())
        .map(|v| v.to_value().to_string())
        .collect();
    assert!(values.len() > 1);
}

#[test]
fn lane_regression_long_strings_and_list_membership() {
    let long = "x".repeat(1000);
    let other = format!("{long}y");
    let rows: Vec<Scope> = [json!({"s": long, "t": long}), json!({"s": long, "t": other}), json!({"s": "a", "t": "a"}), json!({"s": null, "t": long})]
        .into_iter()
        .map(|v| Scope::new(v.into()))
        .collect();
    let mut runner = LaneRunner::new();
    for e in ["s == t", "len(s)", "s + 'z'", "upper(s) == upper(t)", "contains(s, 'xy')", "s[0:2]"] {
        let program = LaneProgram::standard(e).expect("compile");
        for program in [program.clone(), program.specialize(&rows).expect("specialize")] {
            let got: Vec<_> = runner.evaluate(&program, &rows).into_iter().map(|r| r.map(|v| v.to_value()).ok()).collect();
            let want: Vec<_> = rows.iter().map(|s| Isolate::with_environment(s.base().clone()).run_standard(e).map(|v| v.to_value()).ok()).collect();
            assert_eq!(got, want, "{e}");
        }
    }
    let words = ["a", "b", "c", "d"];
    let child = Column::new(Values::Strs(&words));
    let offsets = [0i32, 2, 2, 4, 4];
    let nums = [1i64, 2, 3, 4];
    let num_child = Column::new(Values::I64(&nums));
    let valid = [0b1011u64];
    let texts = ["a", "c", "z", "b"];
    let columns = Columns::new(4)
        .column("ws", Column::with_validity(Values::List { offsets: &offsets, child: (&child).into() }, &valid, 0))
        .column("ns", Column::new(Values::List { offsets: &offsets, child: (&num_child).into() }))
        .column("x", Column::new(Values::Strs(&texts)));
    Rows::check(&columns, &["x in ws", "x not in ws", "'a' in ws", "null in ws", "3 in ns", "x in ns", "1.0 in ns"]);
}

#[test]
fn lane_regression_chained_dotted_keys_resolve() {
    let cases: [(&[(&str, &str)], Value); 6] = [
        (&[("fee.base", "1"), ("fee.total", "$.fee.base + 1"), ("net", "$.fee.total * 2")], json!({})),
        (&[("fee.base", "1"), ("all", "$.fee")], json!({})),
        (&[("a.b", "{x: 1}"), ("c", "$.a.b.x"), ("d", "$.a.b.y ?? 5")], json!({})),
        (&[("a.b", "1"), ("a.c", "$.a.b"), ("d", "$.a")], json!({})),
        (&[("p", "1"), ("q.r", "$.p + $.missing.x ?? 0")], json!({"$": {"missing": {"x": 3}}})),
        (&[("x.y", "2"), ("x.y", "$.x.y + 1"), ("z", "$.x.y")], json!({})),
    ];
    let mut runner = LaneRunner::new();
    for (entries, input) in cases {
        let mut isolate = Isolate::with_environment(input.clone().into());
        let mut expected = Vec::new();
        for (key, source) in entries.iter() {
            let v = isolate.run_standard(source).map(|v| v.to_value()).ok();
            expected.push(v.clone());
            isolate.insert_dollar(key, v.map(Variable::from).unwrap_or(Variable::Null));
        }
        let program = LaneProgram::compile_many(entries, true).expect("compile");
        let scope = Scope::new(input.clone().into());
        let mut got = None;
        runner.evaluate_many(&program, std::slice::from_ref(&scope), None, |_, r| {
            got = r.and_then(|r| r.ok()).map(|v| v.into_iter().map(|x| Some(x.to_value())).collect::<Vec<_>>())
        });
        assert_eq!(got, Some(expected), "{entries:?}");
    }
}

#[test]
fn lane_builtin_failures_keep_messages() {
    let input = json!({"n": 5, "s": "text", "b": true, "l": [1, 2], "o": {"a": 1}});
    let expressions = [
        "contains(n, 'a')",
        "contains(s, 5)",
        "upper(n)",
        "len(b)",
        "startsWith(n, 'x')",
        "sum(s)",
        "number(o)",
        "contains(l, o)",
        "trim(n)",
    ];
    let scope = Scope::new(Variable::from(input.clone()));
    let mut runner = LaneRunner::new();
    let mut mismatches = Vec::new();
    for expression in expressions {
        let mut isolate = Isolate::with_environment(Variable::from(input.clone()));
        let stack = isolate.run_standard(expression).map(|v| v.to_value()).map_err(|e| e.to_string());
        let Ok(program) = LaneProgram::standard(expression) else {
            continue;
        };
        let lane = runner
            .evaluate(&program, std::slice::from_ref(&scope))
            .remove(0)
            .map(|v| v.to_value())
            .map_err(|e| e.to_string());
        if lane != stack {
            mismatches.push(format!("{expression}\n  stack {stack:?}\n  lane  {lane:?}"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn lane_isolated_cells_keep_evaluating_after_a_fault() {
    let program = LaneProgram::compile_cells(&[("> 1", Some("a")), ("x * 2 > 3", None), ("< 10", Some("a")), ("$ == b", Some("a"))])
        .expect("compile");
    let mant = [0i64, 5, 20, 7];
    let scale = [0u8; 4];
    let xs = [Variable::Null, Variable::from(json!(4)), Variable::from(json!("text")), Variable::from(json!(1))];
    let bs = [Variable::from(json!(0)), Variable::from(json!(5)), Variable::Null, Variable::from(json!(8))];
    let columns = Columns::new(4)
        .column("a", Column::new(Values::Scaled { mant: &mant, scale: &scale }))
        .column("x", Column::new(Values::Any(&xs)))
        .column("b", Column::new(Values::Any(&bs)));
    let bind = |key: &str| match columns.find(key) {
        Some(index) => zen_expression::lane::Binding::Column(index),
        None => zen_expression::lane::Binding::Absent,
    };
    let scopes: Vec<Scope> = (0..4).map(|_| Scope::new(Variable::Null)).collect();
    let mut runner = LaneRunner::new();
    let mut outs = Vec::new();
    let mut failures = Vec::new();
    runner.evaluate_bound_into(&program, &scopes, &columns, &bind, None, &mut outs, |row, stage, _| failures.push((row, stage)));
    failures.sort_unstable();
    assert_eq!(failures, vec![(0, 1), (2, 1)]);
    let truth = |cell: usize, row: usize| {
        failures.iter().all(|f| *f != (row, cell))
            && matches!(outs[cell].variable(row), Some(Ok(Variable::Bool(true))))
    };
    let table: Vec<Vec<bool>> = (0..4).map(|cell| (0..4).map(|row| truth(cell, row)).collect()).collect();
    assert_eq!(table[0], vec![false, true, true, true]);
    assert_eq!(table[1], vec![false, true, false, false]);
    assert_eq!(table[2], vec![true, true, false, true]);
    assert_eq!(table[3], vec![true, true, false, false]);
}

enum FieldBuf {
    Scaled(Vec<i64>, Vec<u8>),
    Utf8(Vec<i32>, Vec<u8>),
    Bool(Vec<u64>),
    Any(Vec<Variable>),
    Struct(Vec<(String, FieldBuf, Vec<u64>)>),
}

struct StructLists {
    rows: usize,
    offsets: Vec<i32>,
    valid: Vec<u64>,
    fields: Vec<(String, FieldBuf, Vec<u64>)>,
    items: usize,
    present: Vec<u64>,
    scalars: Vec<(String, Vec<i64>, Vec<u8>)>,
}

impl FieldBuf {
    fn of(cells: &[Option<Value>]) -> (FieldBuf, Vec<u64>) {
        let mut valid = vec![0u64; cells.len().div_ceil(64).max(1)];
        cells.iter().enumerate().filter(|(_, c)| c.is_some()).for_each(|(i, _)| valid[i / 64] |= 1 << (i % 64));
        let present: Vec<&Value> = cells.iter().flatten().collect();
        let buf = if !present.is_empty() && present.iter().all(|v| v.is_object()) {
            let mut names: Vec<String> = Vec::new();
            for v in &present {
                for k in v.as_object().into_iter().flat_map(|m| m.keys()) {
                    if !names.contains(k) {
                        names.push(k.clone());
                    }
                }
            }
            FieldBuf::Struct(
                names
                    .into_iter()
                    .map(|n| {
                        let sub: Vec<Option<Value>> = cells.iter().map(|c| c.as_ref().and_then(|v| v.get(&n)).filter(|v| !v.is_null()).cloned()).collect();
                        let (buf, valid) = FieldBuf::of(&sub);
                        (n, buf, valid)
                    })
                    .collect(),
            )
        } else if !present.is_empty() && present.iter().all(|v| v.is_number()) {
            let parts: Vec<(i64, u8)> = cells
                .iter()
                .map(|c| match c {
                    Some(v) => {
                        let d: Decimal = v.to_string().parse().unwrap_or_default();
                        (i64::try_from(d.mantissa()).unwrap_or(0), d.scale() as u8)
                    }
                    None => (0, 0),
                })
                .collect();
            FieldBuf::Scaled(parts.iter().map(|p| p.0).collect(), parts.iter().map(|p| p.1).collect())
        } else if !present.is_empty() && present.iter().all(|v| v.is_string()) {
            let mut offsets = vec![0i32];
            let mut data = Vec::new();
            for c in cells {
                if let Some(Value::String(s)) = c {
                    data.extend_from_slice(s.as_bytes());
                }
                offsets.push(data.len() as i32);
            }
            FieldBuf::Utf8(offsets, data)
        } else if !present.is_empty() && present.iter().all(|v| v.is_boolean()) {
            let mut bits = vec![0u64; cells.len().div_ceil(64).max(1)];
            cells.iter().enumerate().filter(|(_, c)| matches!(c, Some(Value::Bool(true)))).for_each(|(i, _)| bits[i / 64] |= 1 << (i % 64));
            FieldBuf::Bool(bits)
        } else {
            FieldBuf::Any(cells.iter().map(|c| c.clone().map_or(Variable::Null, Variable::from)).collect())
        };
        (buf, valid)
    }

    fn values<'a>(&'a self, structs: &'a [Vec<(&'a str, Column<'a>)>], next: &mut usize) -> Values<'a> {
        match self {
            FieldBuf::Scaled(mant, scale) => Values::Scaled { mant, scale },
            FieldBuf::Utf8(offsets, data) => Values::Utf8 { offsets, data },
            FieldBuf::Bool(bits) => Values::Bool { bits, offset: 0 },
            FieldBuf::Any(values) => Values::Any(values),
            FieldBuf::Struct(_) => {
                let fields = &structs[*next];
                *next += 1;
                Values::Struct { fields, len: fields.first().map_or(0, |(_, c)| c.len()) }
            }
        }
    }

    fn nested<'a>(&'a self, out: &mut Vec<&'a [(String, FieldBuf, Vec<u64>)]>) {
        if let FieldBuf::Struct(fields) = self {
            for (_, buf, _) in fields {
                buf.nested(out);
            }
            out.push(fields);
        }
    }
}

impl StructLists {
    fn new(rows: &[Value]) -> Self {
        let mut offsets = vec![0i32];
        let mut valid = vec![0u64; rows.len().div_ceil(64)];
        let mut items: Vec<Option<Value>> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if let Some(Value::Array(list)) = row.get("accounts") {
                valid[i / 64] |= 1 << (i % 64);
                items.extend(list.iter().map(|v| Some(v.clone()).filter(|v| !v.is_null())));
            }
            offsets.push(items.len() as i32);
        }
        let (FieldBuf::Struct(fields), present) = FieldBuf::of(&items) else {
            panic!("accounts must hold objects");
        };
        let scalars = ["cap"]
            .iter()
            .map(|k| {
                let parts: Vec<(i64, u8)> = rows
                    .iter()
                    .map(|r| {
                        let d: Decimal = r.get(*k).map(|v| v.to_string()).unwrap_or_default().parse().unwrap_or_default();
                        (i64::try_from(d.mantissa()).unwrap_or(0), d.scale() as u8)
                    })
                    .collect();
                (k.to_string(), parts.iter().map(|p| p.0).collect(), parts.iter().map(|p| p.1).collect())
            })
            .collect();
        Self {
            rows: rows.len(),
            offsets,
            valid,
            fields,
            items: items.len(),
            present,
            scalars,
        }
    }

    fn check(&self, expressions: &[&str]) {
        let mut order: Vec<&[(String, FieldBuf, Vec<u64>)]> = Vec::new();
        for (_, buf, _) in &self.fields {
            buf.nested(&mut order);
        }
        let built: Vec<Vec<(&str, Column)>> = order
            .iter()
            .map(|fields| {
                let mut next = 0usize;
                fields
                    .iter()
                    .map(|(name, buf, valid)| (name.as_str(), Column::with_validity(buf.values(&[], &mut next), valid, 0)))
                    .collect()
            })
            .collect();
        let mut next = 0usize;
        let top: Vec<(&str, Column)> = self
            .fields
            .iter()
            .map(|(name, buf, valid)| (name.as_str(), Column::with_validity(buf.values(&built, &mut next), valid, 0)))
            .collect();
        let account = Column::with_validity(Values::Struct { fields: &top, len: self.items }, &self.present, 0);
        let mut columns = Columns::new(self.rows).column(
            "accounts",
            Column::with_validity(Values::List { offsets: &self.offsets, child: (&account).into() }, &self.valid, 0),
        );
        for (name, mant, scale) in &self.scalars {
            columns = columns.column(name, Column::new(Values::Scaled { mant, scale }));
        }
        let mut runner = LaneRunner::new();
        let mut failures = Vec::new();
        for e in expressions {
            let program = LaneProgram::standard(e).expect("compile");
            for program in [program.clone(), program.specialize_columns(&columns).expect("specialize")] {
                let mut out = Output::new();
                runner.evaluate_columns_into(&program, &columns, &mut out);
                let mut values = Vec::new();
                runner.evaluate_columns(&program, &columns, |_, r| values.push(r.map(|v| v.to_value()).ok()));
                for r in 0..columns.rows {
                    let want = Isolate::with_environment(columns.row(r)).run_standard(e).map(|v| v.to_value()).ok();
                    let got = out.variable(r).and_then(|x| x.ok()).map(|v| v.to_value());
                    if got != want || values.get(r) != Some(&want) {
                        failures.push(format!("{e} row {r}\n  want {want:?}\n  typed {got:?}\n  value {:?}", values.get(r)));
                        break;
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    fn rows(count: usize) -> Vec<Value> {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut n = |m: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % m
        };
        (0..count)
            .map(|i| {
                let accounts: Vec<Value> = (0..n(6))
                    .map(|k| {
                        let mut a = serde_json::Map::new();
                        match n(7) {
                            0 => {}
                            1 => {
                                a.insert("balance".into(), Value::Null);
                            }
                            2 => {
                                a.insert("balance".into(), json!(Decimal::new(n(100000) as i64, 2)));
                            }
                            _ => {
                                a.insert("balance".into(), json!(n(5000)));
                            }
                        }
                        let kind = ["savings", "checking", "loan"][n(3) as usize];
                        a.insert("kind".into(), json!(kind));
                        if n(5) != 0 {
                            a.insert("active".into(), json!(n(3) != 0));
                        }
                        a.insert("x".into(), match n(3) {
                            0 => json!(k),
                            1 => json!("t"),
                            _ => Value::Null,
                        });
                        if n(4) != 0 {
                            let (score, tag) = (n(100), ["a", "b"][n(2) as usize]);
                            a.insert("meta".into(), json!({"score": score, "tag": tag}));
                        }
                        Value::Object(a)
                    })
                    .collect();
                match i % 11 {
                    7 => json!({"cap": n(50)}),
                    9 => json!({"cap": n(50), "accounts": null}),
                    _ => json!({"cap": n(50), "accounts": accounts}),
                }
            })
            .collect()
    }
}

#[test]
fn lane_struct_lists_match_the_stack_vm() {
    let rows = StructLists::rows(150);
    StructLists::new(&rows).check(&[
        "len(accounts)",
        "sum(map(accounts, #.balance ?? 0))",
        "sum(map(filter(accounts, #.active), #.balance ?? 0))",
        "sum(map(filter(accounts, #.active), #.balance))",
        "avg(map(accounts, #.balance))",
        "min(map(filter(accounts, #.active), #.balance))",
        "max(map(accounts, #.balance))",
        "avg(map(filter(accounts, #.active), #.balance))",
        "sum(map(accounts, #.kind))",
        "sum(map(filter(accounts, #.x), #.balance))",
        "sum(map(filter(accounts, #.kind), #.balance))",
        "sum(map(accounts, #.missing))",
        "cap > 10 ? sum(map(accounts, #.balance)) : sum(map(filter(accounts, #.active), #.balance))",
        "count(accounts, #.kind == 'loan')",
        "count(accounts, #.active)",
        "some(accounts, (#.balance ?? 0) > 1000)",
        "all(accounts, #.active)",
        "none(accounts, #.kind == 'loan')",
        "one(accounts, #.kind == 'savings')",
        "map(accounts, #.kind)",
        "map(accounts, upper(#.kind))",
        "map(accounts, (#.balance ?? 0) * 2)",
        "map(accounts, #.balance)",
        "map(accounts, #.active)",
        "filter(accounts, (#.balance ?? 0) > 100)",
        "map(accounts, #)",
        "accounts[0].balance",
        "map(accounts, #.meta.score)",
        "sum(map(accounts, #.meta.score ?? 0))",
        "map(accounts, #.meta)",
        "map(accounts, #.x)",
        "map(accounts, #.missing)",
        "map(accounts, {k: #.kind, b: #.balance})",
        "flatMap(accounts, [#.kind, #.kind])",
        "sum(map(accounts, (#.balance ?? 0) * count(accounts, #.active)))",
        "count(accounts, (#.balance ?? 0) > cap * 10)",
        "map(filter(accounts, #.kind != 'loan'), #.kind)",
        "max(map(accounts, #.balance ?? 0))",
        "sum(map(accounts, #.balance))",
        "len(filter(accounts, #.active))",
        "len(filter(accounts, (#.balance ?? 0) > 1000 and #.active))",
        "count(filter(accounts, #.active), #.kind == 'loan')",
        "map(filter(accounts, #.active), #.balance ?? 0)",
        "some(filter(accounts, #.active), #.kind == 'loan')",
        "all(filter(accounts, #.active), (#.balance ?? 0) >= 0)",
        "filter(filter(accounts, #.active), #.kind == 'loan')",
        "sum(map(filter(filter(accounts, #.active), #.kind != 'loan'), #.balance ?? 0))",
        "len(filter(accounts, #.active)) + count(accounts, #.active)",
        "map(filter(accounts, #.active), #)",
        "map(filter(accounts, #.active), #.meta.tag)",
        "count(accounts, #.meta.tag == 'a')",
        "sum(map(filter(accounts, (#.meta.score ?? 0) > 50), #.meta.score))",
        "map(accounts, #.meta.missing)",
        "map(accounts, (#.meta.score ?? 0) + (#.balance ?? 0))",
        "map(accounts, #.meta == null)",
        "map(accounts, #.meta.score > 10 and #.active)",
        "accounts[1].kind",
        "accounts[0].meta.score",
        "accounts[5].balance",
        "accounts[0]",
        "accounts[0].missing",
        "accounts[0].meta",
        "(accounts[0].balance ?? 0) + (accounts[1].balance ?? 0)",
    ]);
}

#[test]
fn lane_struct_lists_with_null_items() {
    let rows: Vec<Value> = (0..90)
        .map(|i| match i % 5 {
            0 => json!({"cap": i, "accounts": [{"balance": i, "kind": "loan", "active": true}, null, {"balance": 3, "kind": "savings"}]}),
            1 => json!({"cap": i, "accounts": [null]}),
            2 => json!({"cap": i, "accounts": []}),
            3 => json!({"cap": i}),
            _ => json!({"cap": i, "accounts": [{"balance": 1.5, "kind": "checking", "active": false}]}),
        })
        .collect();
    StructLists::new(&rows).check(&[
        "map(accounts, #.balance)",
        "sum(map(accounts, #.balance ?? 0))",
        "count(accounts, #.kind == 'loan')",
        "map(accounts, #)",
        "len(filter(accounts, #.active))",
        "map(filter(accounts, #.kind != 'loan'), #.kind)",
        "some(accounts, # == null)",
    ]);
}

struct Fused;

impl Fused {
    fn rows(count: usize) -> Vec<Value> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut n = |m: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % m
        };
        (0..count)
            .map(|_| {
                let mut row = serde_json::Map::new();
                for key in ["x", "y"] {
                    match n(9) {
                        0 => {}
                        1 => {
                            row.insert(key.into(), Value::Null);
                        }
                        2 => {
                            row.insert(key.into(), serde_json::from_str(&Decimal::new(n(100_000) as i64 - 50_000, 3).to_string()).unwrap_or_default());
                        }
                        _ => {
                            row.insert(key.into(), json!(n(40) as i64 - 10));
                        }
                    }
                }
                if n(6) != 0 {
                    row.insert("s".into(), json!(["gold", "silver", "bronze", "", "goldfish"][n(5) as usize]));
                }
                Value::Object(row)
            })
            .collect()
    }

    fn columns<'a>(rows: &[Value], numbers: &'a mut Vec<(Vec<i64>, Vec<u8>, Vec<u64>)>, text: &'a mut (Vec<i32>, Vec<u8>, Vec<u64>)) -> Columns<'a> {
        for key in ["x", "y"] {
            let mut column = (Vec::new(), Vec::new(), vec![0u64; rows.len().div_ceil(64)]);
            for (r, row) in rows.iter().enumerate() {
                let d: Option<Decimal> = row.get(key).filter(|v| v.is_number()).and_then(|v| v.to_string().parse().ok());
                let (m, s) = d.map_or((0, 0), |d| (i64::try_from(d.mantissa()).unwrap_or(0), d.scale() as u8));
                column.0.push(m);
                column.1.push(s);
                if d.is_some() {
                    column.2[r / 64] |= 1 << (r % 64);
                }
            }
            numbers.push(column);
        }
        text.0.push(0);
        text.2.resize(rows.len().div_ceil(64), 0);
        for (r, row) in rows.iter().enumerate() {
            if let Some(Value::String(v)) = row.get("s") {
                text.1.extend_from_slice(v.as_bytes());
                text.2[r / 64] |= 1 << (r % 64);
            }
            text.0.push(text.1.len() as i32);
        }
        let mut columns = Columns::new(rows.len());
        for (key, (mant, scale, valid)) in ["x", "y"].iter().zip(numbers.iter()) {
            columns = columns.column(key, Column::with_validity(Values::Scaled { mant, scale }, valid, 0));
        }
        columns.column("s", Column::with_validity(Values::Utf8 { offsets: &text.0, data: &text.1 }, &text.2, 0))
    }

    fn expected(env: Variable, entries: &[Fusion]) -> Option<Vec<Value>> {
        let mut outputs = Vec::new();
        for entry in entries {
            let mut isolate = Isolate::with_environment(env.depth_clone(usize::MAX));
            let (key, value) = match entry {
                Fusion::Output { key, source } | Fusion::Hidden { key, source } => (key, isolate.run_standard(source).ok()?),
                Fusion::Rules { key, rules } => {
                    let mut chosen = Variable::Null;
                    for (cells, value) in rules {
                        let mut matched = true;
                        for (field, cell) in cells {
                            let reference = isolate.run_standard(field).ok()?;
                            isolate.set_reference_value(reference).ok()?;
                            if !isolate.run_unary(cell).ok()? {
                                matched = false;
                                break;
                            }
                        }
                        if matched {
                            chosen = isolate.run_standard(value).ok()?;
                            break;
                        }
                    }
                    (key, chosen)
                }
            };
            if !key.is_empty() {
                env.dot_insert(key, value.clone());
            }
            if !matches!(entry, Fusion::Hidden { .. }) {
                outputs.push(value.to_value());
            }
        }
        Some(outputs)
    }

    fn check(rows: &[Value], entries: &[Fusion]) {
        let (mut numbers, mut text) = (Vec::new(), (Vec::new(), Vec::new(), Vec::new()));
        let columns = Self::columns(rows, &mut numbers, &mut text);
        let generic = LaneProgram::compile_fused(entries).expect("fused program");
        let mut runner = LaneRunner::new();
        for program in [generic.clone(), generic.specialize_columns(&columns).expect("specialize")] {
            let mut outs: Vec<Output> = Vec::new();
            let mut failed = vec![false; rows.len()];
            runner.evaluate_columns_many(&program, &columns, &mut outs, |row, _, _| failed[row] = true);
            for (r, row) in rows.iter().enumerate() {
                let want = Self::expected(columns.row(r), entries);
                let got = match failed[r] {
                    true => None,
                    false => outs.iter().map(|o| o.variable(r).and_then(|v| v.ok()).map(|v| v.to_value())).collect::<Option<Vec<_>>>(),
                };
                assert_eq!(got, want, "row {r} {row}");
            }
        }
    }

    fn output(key: &str, source: &str) -> Fusion {
        Fusion::Output { key: key.to_string(), source: source.to_string() }
    }
}

#[test]
fn lane_fused_programs_match_sequential_evaluation() {
    let rows = Fused::rows(700);
    Fused::check(
        &rows,
        &[
            Fused::output("a.total", "(x ?? 0) + (y ?? 0)"),
            Fused::output("a.flag", "a.total > 10"),
            Fused::output("", "a.total * 2"),
            Fused::output("a.label", "a.flag ? 'big' : (a.total > 0 ? 'mid' : 'small')"),
            Fused::output("a.net", "a.total - (y ?? 1) - (x ?? 2)"),
            Fused::output("a.n", "a.label == 'big' ? a.net : -1"),
            Fused::output("a.total", "a.total * 3"),
            Fused::output("b", "a.total + a.n.missing"),
            Fused::output("c", "s == 'gold' and a.flag"),
            Fused::output("d", "s == 'gold' ? 1 : (s == 'silver' ? 2.5 : (s == '' ? -1 : 0))"),
            Fused::output("e", "((s == 'goldfish') == true) ? 'F' : (((s == 'bronze') == true) ? 'B' : null)"),
            Fused::output("f", "s == 'gold' ? true : (s == 'bronze' ? false : true)"),
            Fused::output("g", "a.label == 'big' ? 'B' : (a.label == 'mid' ? 'M' : 'S')"),
            Fused::output("", "x / y"),
        ],
    );
    Fused::check(
        &rows,
        &[
            Fused::output("a.risk", "(x ?? 0) > 5 ? 'high' : ((x ?? 0) > 0 ? 'mid' : 'low')"),
            Fusion::Hidden { key: "__fuse0".to_string(), source: "upper(s ?? '')".to_string() },
            Fusion::Rules {
                key: "a.discount".to_string(),
                rules: vec![
                    (vec![("s".to_string(), "'gold'".to_string()), ("a.risk".to_string(), "'low'".to_string())], "0.15".to_string()),
                    (vec![("s".to_string(), "'gold'".to_string()), ("a.risk".to_string(), "'mid'".to_string())], "0.1".to_string()),
                    (vec![("__fuse0".to_string(), "'SILVER'".to_string())], "x".to_string()),
                    (vec![("a.risk".to_string(), "'low', 'mid'".to_string()), ("y".to_string(), "> 3".to_string())], "y * 2".to_string()),
                    (vec![], "0".to_string()),
                ],
            },
            Fused::output("a.net", "(x ?? 0) - a.discount"),
            Fused::output("a.eligible", "a.risk != 'high' and a.discount > 0"),
        ],
    );
}

#[test]
fn lane_fused_programs_reject_unresolvable_reads() {
    for entries in [
        vec![Fused::output("a.total", "x + 1"), Fused::output("b", "a")],
        vec![Fused::output("a.total", "x + 1"), Fused::output("b", "map([1, 2], a.total + #)")],
        vec![Fused::output("a.total", "x + 1"), Fused::output("b", "$root")],
        vec![Fused::output("a.total", "x + 1"), Fused::output("a", "2")],
        vec![Fused::output("a", "x + 1"), Fused::output("a.total", "2")],
    ] {
        assert!(LaneProgram::compile_fused(&entries).is_err());
    }
}

#[test]
fn lane_switches_over_coded_and_plain_strings() {
    let texts = ["gold", "silver", "", "goldfish", "a-much-longer-tier-name"];
    let offsets: Vec<i32> = std::iter::once(0)
        .chain(texts.iter().scan(0i32, |at, t| {
            *at += t.len() as i32;
            Some(*at)
        }))
        .collect();
    let data: String = texts.concat();
    let dictionary = Column::new(Values::Text { offsets: &offsets, data: &data });
    let keys: Vec<i32> = (0..300).map(|i| (i * 7 % 6) as i32 - 1).collect();
    let valid: Vec<u64> = vec![u64::MAX ^ 0b1001, u64::MAX, u64::MAX ^ (1 << 40), u64::MAX, u64::MAX];
    let mut plain_offsets = vec![0i32];
    let mut plain = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        if let Some(text) = usize::try_from(*key).ok().and_then(|k| texts.get(k)) {
            if i % 5 != 3 {
                plain.extend_from_slice(text.as_bytes());
            }
        }
        plain_offsets.push(plain.len() as i32);
    }
    let columns = Columns::new(300)
        .column("t", Column::with_validity(Values::Dict { keys: &keys, values: (&dictionary).into() }, &valid, 0))
        .column("u", Column::with_validity(Values::Utf8 { offsets: &plain_offsets, data: &plain }, &valid, 0));
    let expressions = [
        "t == 'gold' ? 1 : (t == 'silver' ? 2 : 3)",
        "t == 'goldfish' ? 'F' : (t == '' ? 'E' : (t == 'a-much-longer-tier-name' ? 'L' : 'O'))",
        "t == 'gold' ? true : (t == 'gold' ? false : null)",
        "u == 'gold' ? 1 : (u == 'silver' ? 2 : 3)",
        "u == 'goldfish' ? 'F' : (u == '' ? 'E' : (u == 'a-much-longer-tier-name' ? 'L' : 'O'))",
        "(u == 'gold' ? 1 : (u == 'silver' ? 2 : 3)) + (t == 'gold' ? 10 : (t == 'silver' ? 20 : 30))",
    ];
    for expression in expressions.iter().take(5) {
        let program = LaneProgram::standard(expression).expect("compile");
        let switched = program.program().steps.iter().any(|step| matches!(step.op, zen_expression::lane::Op::Switch(_)));
        assert!(switched, "{expression}");
    }
    Rows::check(&columns, &expressions);
}

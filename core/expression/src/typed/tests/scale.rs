use crate::typed::build::Passes;
use crate::typed::tests::data::{ColSpec, Enc, Frame, ListSpec, Mock, Row, Spec, Value};
use crate::typed::tests::oracle::{Oracle, Val};
use crate::typed::tests::workload::{Block, Dialect, Workload};
use crate::typed::{Class, Data, Dec, Hir, Hit, Lowering, Program, Scope, TableRule, TableSpec};

struct Case {
    workload: Workload,
    hir: Hir,
    program: Program,
}

impl Case {
    fn new(spec: Spec, blocks: &[(&str, &str)]) -> Self {
        let blocks = blocks.iter().map(|(k, s)| Block::Expr { key: k.to_string(), source: s.to_string() }).collect();
        let workload = Workload { name: "case", spec, dialect: Dialect::Policy, member: None, blocks };
        let mut lowering = Lowering::new(None);
        let mut mock = Mock { spec: &workload.spec, member: None };
        for block in &workload.blocks {
            block.lower(&mut lowering, &mut mock, Hit::Strict).expect("lowering");
        }
        let hir = lowering.finish();
        let program = Program::new(hir.clone()).expect("program");
        Self { workload, hir, program }
    }

    fn nums(names: &[&'static str], enc: Enc) -> Spec {
        Spec { columns: names.iter().map(|n| ColSpec::new(n, Class::Num, enc)).collect(), lists: Vec::new() }
    }

    fn run(&self, rows: &[Row], select: Option<&[u64]>) -> Vec<Option<Vec<String>>> {
        let frame = Frame::new(&self.hir, &self.workload.spec, rows, 0);
        let mut out = self.program.outputs();
        self.program.run(&frame.batch(), &mut out, select).expect("run");
        (0..rows.len())
            .map(|r| {
                (!out.faults().get(r)).then(|| {
                    out.slots()
                        .iter()
                        .map(|s| match (&s.data, s.scope) {
                            (Data::Num { .. }, Scope::Row) => s.decimal(r).map_or("null".to_string(), |d| d.to_string()),
                            (Data::Bool(_), Scope::Row) => s.boolean(r).map_or("null".to_string(), |b| b.to_string()),
                            _ => "?".to_string(),
                        })
                        .collect()
                })
            })
            .collect()
    }

    fn vm(&self, rows: &[Row]) -> Vec<Option<Vec<String>>> {
        let oracle = Oracle::new(&self.workload, &self.hir);
        rows.iter().map(|r| oracle.run(r.variable(&self.workload.spec)).ok().map(|v| v.iter().map(Val::to_string).collect())).collect()
    }

    fn check(&self, rows: &[Row]) -> Vec<Option<Vec<String>>> {
        let typed = self.run(rows, None);
        let vm = self.vm(rows);
        for (r, (t, v)) in typed.iter().zip(&vm).enumerate() {
            if let Some(t) = t {
                assert_eq!(Some(t), v.as_ref(), "row {r}: {:?}", rows[r]);
            }
        }
        typed
    }

    fn d(m: i64, s: u8) -> Value {
        Value::Num(Dec::new(m, s))
    }

    fn row(values: &[Value]) -> Row {
        Row { columns: values.to_vec(), lists: Vec::new() }
    }
}

#[test]
fn zero_operand_scale() {
    let blocks = [("x.sum", "a + b"), ("x.diff", "a - b"), ("x.prod", "a * b"), ("x.lit", "a + 5"), ("x.cmp", "a == b")];
    let fixed = [Case::row(&[Case::d(0, 2), Case::d(500, 2)]), Case::row(&[Case::d(150, 2), Case::d(0, 2)]), Case::row(&[Case::d(0, 2), Case::d(0, 2)])];
    let mixed = [Case::row(&[Case::d(0, 2), Case::d(5, 0)]), Case::row(&[Case::d(5, 0), Case::d(0, 2)]), Case::row(&[Case::d(0, 2), Case::d(0, 3)]), Case::row(&[Case::d(150, 2), Case::d(2, 0)])];
    for enc in [Enc::Scaled, Enc::Dec] {
        let case = Case::new(Case::nums(&["a", "b"], enc), &blocks);
        let out = case.check(&fixed);
        assert_eq!(out[0].as_deref(), Some(&["5.00", "-5.00", "0", "5", "false"].map(String::from)[..]));
        assert_eq!(out[2].as_deref(), Some(&["0.00", "0.00", "0", "5", "true"].map(String::from)[..]));
        let out = case.check(&mixed);
        assert_eq!(out[0].as_deref(), Some(&["5", "-5", "0", "5", "false"].map(String::from)[..]));
        assert_eq!(out[1].as_deref(), Some(&["5", "5", "0", "10", "false"].map(String::from)[..]));
        assert_eq!(out[2].as_deref(), Some(&["0.000", "0.000", "0", "5", "true"].map(String::from)[..]));
        assert_eq!(out[3].as_deref(), Some(&["3.50", "-0.50", "3.00", "6.50", "false"].map(String::from)[..]));
    }
}

#[test]
fn division_scale_matches_decimal() {
    let blocks = [("x.pow", "a / 100"), ("x.one", "a / 10"), ("x.quarter", "a / 4"), ("x.div", "a / b"), ("x.zero", "a / 0")];
    let rows: Vec<Row> = [(150, 0), (300, 0), (0, 0), (1, 0), (7, 0), (12345, 2), (-150, 0), (15, 1), (1, 28), (10, 0)]
        .iter()
        .zip([4, 3, 7, 1, 0, 8, -2, 5, 16, 3])
        .map(|((m, s), b)| Case::row(&[Case::d(*m, *s), Case::d(b, 0)]))
        .collect();
    let case = Case::new(Case::nums(&["a", "b"], Enc::Scaled), &blocks);
    let out = case.check(&rows);
    assert_eq!(out[0].as_ref().map(|v| v[0].as_str()), Some("1.50"));
    assert_eq!(out[1].as_ref().map(|v| v[0].as_str()), Some("3"));
    assert_eq!(out[2].as_ref().map(|v| v[0].as_str()), Some("0"));
    assert_eq!(out[4].as_ref().map(|v| v[3].as_str()), Some("null"));
    assert!(out.iter().take(4).all(Option::is_some), "{out:?}");
    let fixed: Vec<Row> = (0..200).map(|i| Case::row(&[Case::d(i * 37 - 3000, 0), Case::d(i % 9 - 4, 0)])).collect();
    case.check(&fixed);
}

#[test]
fn empty_list_sum_has_scale_zero() {
    let spec = Spec {
        columns: Vec::new(),
        lists: vec![ListSpec {
            path: "c.items",
            alias: "item",
            fields: vec![ColSpec::new("fee", Class::Num, Enc::Scaled), ColSpec::new("on", Class::Bool, Enc::Bool)],
        }],
    };
    let case = Case::new(spec, &[("c.sum", "sum(map(c.items, #.fee))"), ("c.kept", "sum(map(filter(c.items, #.on), #.fee))")]);
    let item = |m: i64, s: u8, on: bool| vec![Case::d(m, s), Value::Bool(on)];
    let rows = vec![
        Row { columns: Vec::new(), lists: vec![Vec::new()] },
        Row { columns: Vec::new(), lists: vec![vec![item(0, 2, true)]] },
        Row { columns: Vec::new(), lists: vec![vec![item(125, 2, false), item(0, 2, true)]] },
        Row { columns: Vec::new(), lists: vec![vec![item(125, 2, false), item(5, 2, false)]] },
    ];
    let out = case.check(&rows);
    let strs = |r: usize| out[r].clone().unwrap_or_default();
    assert_eq!(strs(0), ["0", "0"]);
    assert_eq!(strs(1), ["0.00", "0.00"]);
    assert_eq!(strs(2), ["1.25", "0.00"]);
    assert_eq!(strs(3), ["1.30", "0"]);
    let mixed: Vec<Row> = rows.iter().cloned().chain([Row { columns: Vec::new(), lists: vec![vec![item(5, 1, true), item(0, 3, true)]] }]).collect();
    case.check(&mixed);
}

#[test]
fn dyn_sums_match_vm() {
    let spec = Spec {
        columns: Vec::new(),
        lists: vec![ListSpec {
            path: "c.items",
            alias: "item",
            fields: vec![ColSpec::new("fee", Class::Num, Enc::Scaled), ColSpec::new("on", Class::Bool, Enc::Bool)],
        }],
    };
    let case = Case::new(spec, &[("c.sum", "sum(map(c.items, #.fee))"), ("c.kept", "sum(map(filter(c.items, #.on), #.fee))")]);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut pick = |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    let mants = [0i64, 1, -1, 5, 15, -15, 150, -150, 1500, 7, 120, -120, 999_999];
    let rows: Vec<Row> = (0..900)
        .map(|r| {
            let len = match r % 97 {
                0 => 700,
                1 => 1100,
                _ => pick(9) as usize,
            };
            let items = (0..len)
                .map(|_| vec![Case::d(mants[pick(mants.len() as u64) as usize], pick(5) as u8), Value::Bool(pick(3) != 0)])
                .collect();
            Row { columns: Vec::new(), lists: vec![items] }
        })
        .collect();
    let out = case.check(&rows);
    assert!(out.iter().filter(|r| r.is_none()).count() < rows.len() / 50, "too many faulted rows");
    let cancel = vec![
        Row { columns: Vec::new(), lists: vec![vec![vec![Case::d(150, 2), Value::Bool(true)], vec![Case::d(-15, 1), Value::Bool(true)], vec![Case::d(7, 0), Value::Bool(true)]]] },
        Row { columns: Vec::new(), lists: vec![vec![vec![Case::d(150, 2), Value::Bool(true)], vec![Case::d(-15, 1), Value::Bool(true)], vec![Case::d(0, 3), Value::Bool(true)]]] },
        Row { columns: Vec::new(), lists: vec![vec![vec![Case::d(0, 3), Value::Bool(true)], vec![Case::d(0, 1), Value::Bool(false)]]] },
        Row { columns: Vec::new(), lists: vec![vec![vec![Case::d(i64::MAX, 0), Value::Bool(true)], vec![Case::d(1, 2), Value::Bool(true)]]] },
    ];
    let out = case.check(&cancel);
    assert_eq!(out[0].as_deref(), Some(&["7", "7"].map(String::from)[..]));
    assert_eq!(out[1].as_deref(), Some(&["0.000", "0.000"].map(String::from)[..]));
    assert_eq!(out[2].as_deref(), Some(&["0.0", "0.000"].map(String::from)[..]));
    assert_eq!(out[3].as_deref(), Some(&["9223372036854775807.01", "9223372036854775807.01"].map(String::from)[..]));
}

#[test]
fn negating_zero_is_exact() {
    let case = Case::new(Case::nums(&["a"], Enc::Scaled), &[("x.neg", "-a"), ("x.lit", "-(a * 0)"), ("x.sum", "-a + 1")]);
    let out = case.check(&[Case::row(&[Case::d(5, 1)]), Case::row(&[Case::d(0, 2)]), Case::row(&[Case::d(i64::MIN, 0)])]);
    assert!(out.iter().all(Option::is_some), "{out:?}");
    assert_eq!(out[2].as_ref().map(|v| v[0].as_str()), Some("9223372036854775808"));
    let case = Case::new(Case::nums(&["a"], Enc::Scaled), &[("x.neg", "-a")]);
    let out = case.check(&[Case::row(&[Case::d(5, 1)]), Case::row(&[Case::d(0, 2)]), Case::row(&[Value::Null])]);
    assert_eq!(out[0].as_deref(), Some(&["-0.5".to_string()][..]));
    assert!(out[1].is_some());
    assert!(out[2].is_none());
}

#[test]
fn selection_mask_limits_faults() {
    let case = Case::new(Case::nums(&["a"], Enc::Scaled), &[("x.big", "a * 1000000000000")]);
    let rows: Vec<Row> = [i64::MAX / 10, 7, i64::MAX / 10, 3].iter().map(|m| Case::row(&[Case::d(*m, 0)])).collect();
    assert_eq!(case.run(&rows, None).iter().filter(|r| r.is_none()).count(), 2);
    let select = [0b0001u64];
    let out = case.run(&rows, Some(&select));
    assert_eq!(out.iter().map(Option::is_none).collect::<Vec<_>>(), [true, false, false, false]);
}

#[test]
fn member_outputs_with_offset_lists() {
    let workload = Workload::get("W1");
    let hir = workload.hir().expect("lowering");
    let rows = workload.mixed(100);
    let program = Program::new(hir.clone()).expect("program");
    let oracle = Oracle::new(&workload, &hir);
    for shift in [0, 5, 64, 70] {
        let frame = Frame::new(&hir, &workload.spec, &rows, shift);
        let mut out = program.outputs();
        program.run(&frame.batch(), &mut out, None).expect("run");
        for (r, row) in rows.iter().enumerate() {
            if out.faults().get(r) {
                continue;
            }
            let want = oracle.run(row.variable(&workload.spec)).expect("vm");
            let Val::List(fees) = &want[0] else {
                panic!("fee list");
            };
            let got: Vec<String> = frame.range(0, r).map(|e| out.slots()[0].decimal(e).map_or("null".to_string(), |d| d.to_string())).collect();
            assert_eq!(got, fees.iter().map(Val::to_string).collect::<Vec<_>>(), "shift {shift} row {r}");
        }
    }
}

#[test]
fn outputs_move_out() {
    let case = Case::new(Case::nums(&["a"], Enc::Scaled), &[("x.a", "a + 1"), ("x.b", "a > 1")]);
    let frame = Frame::new(&case.hir, &case.workload.spec, &[Case::row(&[Case::d(1, 0)]), Case::row(&[Value::Null])], 0);
    let mut out = case.program.outputs();
    case.program.run(&frame.batch(), &mut out, None).expect("run");
    let (mant, scale, valid) = out.slot_mut(0).and_then(|s| s.take_num()).expect("num");
    assert_eq!((mant[0], scale[0], valid[0] & 1), (2, 0, 1));
    assert!(out.faults().get(1));
    let (bits, _) = out.slot_mut(1).and_then(|s| s.take_bool()).expect("bool");
    assert_eq!(bits[0] & 1, 0);
    case.program.run(&frame.batch(), &mut out, None).expect("rerun");
    assert_eq!(out.slots()[0].num(0), Some(Dec::int(2)));
}

#[test]
fn rules_fault_strictly() {
    let spec = Spec { columns: vec![ColSpec::new("c.score", Class::Num, Enc::Scaled), ColSpec::new("c.tier", Class::Text, Enc::Text)], lists: Vec::new() };
    let rule = |score: Option<&str>, tier: Option<&str>, value: &str| TableRule { cells: vec![score.map(str::to_string), tier.map(str::to_string)], values: vec![Some(value.to_string())] };
    for hit in [Hit::Strict, Hit::Lenient] {
        let table = TableSpec {
            inputs: vec![Some("c.score".to_string()), Some("c.tier".to_string())],
            outputs: vec!["c.pick".to_string()],
            rules: vec![rule(Some("> 10"), Some("'gold'"), "1"), rule(None, Some("'silver'"), "2"), rule(None, None, "3")],
            hit,
            selector: false,
        };
        let mut lowering = Lowering::new(None);
        lowering.table(&table, &mut Mock { spec: &spec, member: None }).expect("lowering");
        let hir = lowering.finish();
        let looked = Passes { lookup_min: 1, ..Passes::ALL };
        let rows = [
            Case::row(&[Case::d(20, 0), Value::Str("gold".into())]),
            Case::row(&[Case::d(5, 0), Value::Str("silver".into())]),
            Case::row(&[Value::Null, Value::Str("silver".into())]),
            Case::row(&[Case::d(20, 0), Value::Null]),
            Case::row(&[Case::d(20, 0), Value::Str("bronze".into())]),
        ];
        for program in [Program::new(hir.clone()).expect("program"), Program::with(hir.clone(), looked).expect("program")] {
            let frame = Frame::new(&hir, &spec, &rows, 0);
            let mut out = program.outputs();
            program.run(&frame.batch(), &mut out, None).expect("run");
            let got: Vec<String> = (0..rows.len())
                .map(|r| if out.faults().get(r) { "fault".to_string() } else { out.slots()[0].decimal(r).map_or("null".to_string(), |d| d.to_string()) })
                .collect();
            match hit {
                Hit::Strict => assert_eq!(got, ["1", "2", "fault", "3", "3"]),
                Hit::Lenient => assert_eq!(got, ["1", "2", "2", "3", "3"]),
            }
        }
    }
}

struct Bounds;

impl Bounds {
    fn sync<T: Send + Sync>() {}
}

#[test]
fn programs_share_across_threads() {
    Bounds::sync::<Program>();
    let workload = Workload::get("W2");
    let hir = workload.hir().expect("lowering");
    let rows = workload.mixed(400);
    let program = Program::new(hir.clone()).expect("program");
    let oracle = Oracle::new(&workload, &hir);
    let expected: Vec<String> = rows.iter().map(|r| oracle.run(r.variable(&workload.spec)).map(|v| format!("{v:?}")).unwrap_or_default()).collect();
    std::thread::scope(|scope| {
        for size in [400, 64, 7, 1] {
            let (program, rows, hir, spec, expected) = (&program, &rows, &hir, &workload.spec, &expected);
            scope.spawn(move || {
                for (c, chunk) in rows.chunks(size).enumerate() {
                    let frame = Frame::new(hir, spec, chunk, 0);
                    let mut out = program.outputs();
                    program.run(&frame.batch(), &mut out, None).expect("run");
                    for r in (0..chunk.len()).filter(|r| !out.faults().get(*r)) {
                        let got: Vec<Val> = out
                            .slots()
                            .iter()
                            .map(|s| match &s.data {
                                Data::Num { .. } => s.decimal(r).map_or(Val::Null, Val::Num),
                                Data::Bool(_) => s.boolean(r).map_or(Val::Null, Val::Bool),
                                Data::Code(_) => s.code(r).zip(s.dict()).map_or(Val::Null, |(c, d)| Val::Str(hir.dict(d).text(c).unwrap_or("?").to_string())),
                            })
                            .collect();
                        assert_eq!(format!("{got:?}"), expected[c * size + r], "size {size} row {}", c * size + r);
                    }
                }
            });
        }
    });
}

#[test]
fn signature_cap_falls_back_to_generic() {
    let case = Case::new(Case::nums(&["a", "b"], Enc::Scaled), &[("x.sum", "a + b"), ("x.prod", "a * b"), ("x.div", "a / 100"), ("x.cmp", "a > b")]);
    for scale in 0..24u8 {
        let rows: Vec<Row> = (0..40).map(|i| Case::row(&[Case::d(i * 7 - 100, scale), Case::d(i % 5, 2)])).collect();
        case.check(&rows);
    }
}

#[test]
fn wide_lanes_stay_exact() {
    let blocks = [("x.third", "a / 3"), ("x.round", "round(a / 3, 2)"), ("x.big", "a * 1000000000"), ("x.back", "a * 1000000000 / 1000000000"), ("x.cmp", "a / 3 > b")];
    let case = Case::new(Case::nums(&["a", "b"], Enc::Scaled), &blocks);
    let rows: Vec<Row> = [(10, 0), (i64::MAX, 0), (-7, 2), (0, 3), (1, 28), (i64::MIN, 0)].iter().map(|(m, s)| Case::row(&[Case::d(*m, *s), Case::d(3, 0)])).collect();
    let out = case.check(&rows);
    assert!(out.iter().all(Option::is_some), "{out:?}");
    assert_eq!(out[0].as_ref().map(|v| v[0].as_str()), Some("3.3333333333333333333333333333"));
    assert_eq!(out[0].as_ref().map(|v| v[1].as_str()), Some("3.33"));
    let frame = Frame::new(&case.hir, &case.workload.spec, &rows, 0);
    let mut out = case.program.outputs();
    case.program.run(&frame.batch(), &mut out, None).expect("run");
    assert!(!out.slots()[0].wide.is_empty());
    assert!(!out.slots()[2].wide.is_empty());
}

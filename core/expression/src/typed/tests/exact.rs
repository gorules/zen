use crate::typed::batch::Feeds;
use crate::typed::build::Passes;
use crate::typed::tests::data::{Enc, Frame, Row, Value};
use crate::typed::tests::interp::Interp;
use crate::typed::tests::oracle::{Oracle, RowResult, Val};
use crate::typed::tests::workload::Workload;
use crate::typed::{Data, Dec, Hir, Kind, Outputs, Program, Scope, Slot};
use std::time::Instant;

struct Lab {
    workload: Workload,
    hir: Hir,
    rows: Vec<Row>,
    expected: Vec<RowResult>,
}

#[derive(Debug, Default)]
struct Report {
    faults: usize,
    interp: usize,
    mismatches: usize,
    spurious: usize,
    samples: Vec<String>,
}

impl Lab {
    fn new(workload: Workload, rows: Vec<Row>) -> Self {
        Workload::mock();
        let hir = workload.hir().expect("lowering");
        let oracle = Oracle::new(&workload, &hir);
        let expected = rows.iter().map(|r| oracle.run(r.variable(&workload.spec))).collect();
        Self { workload, hir, rows, expected }
    }

    fn val(slot: &Slot, hir: &Hir, i: usize) -> Val {
        match (&slot.data, slot.kind) {
            (Data::Num { .. }, _) => slot.decimal(i).map_or(Val::Null, Val::Num),
            (Data::Bool(_), _) => slot.boolean(i).map_or(Val::Null, Val::Bool),
            (Data::Code(_), Kind::Code(d)) => {
                slot.code(i).map_or(Val::Null, |c| hir.dict(d).text(c).map_or(Val::Other(format!("code {c}")), |t| Val::Str(t.to_string())))
            }
            (Data::Code(_), _) => Val::Other("code without dict".to_string()),
            (Data::Text { .. }, _) => slot.text(i).map_or(Val::Null, |t| Val::Str(t.to_string())),
            (Data::Date(_), _) => match slot.date(i) {
                Some(_) => Val::Other(format!("{}", slot.variable(i).to_value())),
                None => Val::Null,
            },
        }
    }

    fn rows_of(out: &Outputs, hir: &Hir, frame: &Frame) -> Vec<Option<Vec<Val>>> {
        (0..frame.rows)
            .map(|row| {
                (!out.faults().get(row)).then(|| {
                    out.slots()
                        .iter()
                        .map(|slot| match (slot.offsets.as_deref(), slot.scope) {
                            (_, Scope::Row) => Self::val(slot, hir, row),
                            (None, Scope::Elem(l)) => Val::List(frame.elements(l as usize, row).into_iter().map(|e| Self::val(slot, hir, e)).collect()),
                            (Some(offsets), Scope::Elem(l)) => {
                                let items = |o: usize| Val::List((offsets[o] as usize..offsets[o + 1] as usize).map(|e| Self::val(slot, hir, e)).collect());
                                match hir.schema.owner(Scope::Elem(l)) {
                                    Some(Scope::Elem(p)) => Val::List(frame.elements(p as usize, row).into_iter().map(items).collect()),
                                    _ => items(row),
                                }
                            }
                        })
                        .collect()
                })
            })
            .collect()
    }

    fn same(got: &[Val], want: &[Val]) -> bool {
        got.len() == want.len() && got.iter().zip(want).all(|(a, b)| a.to_string() == b.to_string())
    }

    fn verify(&self, program: &Program, size: usize) -> Report {
        let mut report = Report::default();
        let mut out = program.outputs();
        for (chunk, start) in self.rows.chunks(size).zip((0..).step_by(size)) {
            let shift = if size == 7 { 3 } else { 0 };
            let frame = Frame::new(&self.hir, &self.workload.spec, chunk, shift);
            let batch = frame.batch();
            program.run(&batch, &mut out, None).expect("run");
            let typed = Self::rows_of(&out, &self.hir, &frame);
            let mut feeds = Feeds::new(&self.hir, &[], &batch).expect("feeds");
            feeds.now = crate::lane::date::Date::now();
            let interp = Interp::run(&self.hir, &feeds);
            for (i, (t, r)) in typed.iter().zip(&interp).enumerate() {
                let row = start + i;
                let want = &self.expected[row];
                report.faults += t.is_none() as usize;
                report.spurious += (t.is_none() && want.is_ok()) as usize;
                report.interp += r.is_none() as usize;
                let mut check = |got: &Option<Vec<Val>>, who: &str| {
                    let ok = match (got, want) {
                        (None, _) => true,
                        (Some(g), Ok(w)) => Self::same(g, w),
                        (Some(_), Err(_)) => false,
                    };
                    if !ok {
                        report.mismatches += 1;
                        if report.samples.len() < 5 {
                            let show = |v: &[Val]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" | ");
                            report.samples.push(format!(
                                "{who} row {row} {:?}: got {} want {}",
                                self.rows[row],
                                got.as_deref().map_or("fault".to_string(), show),
                                want.as_deref().map_or_else(|e| format!("error {e}"), show)
                            ));
                        }
                    }
                };
                check(t, "typed");
                check(r, "interp");
                if t.is_some() != r.is_some() && report.samples.len() < 5 {
                    report.samples.push(format!("fault disagreement row {row}: typed {} interp {}", t.is_none(), r.is_none()));
                }
            }
        }
        report
    }

    fn check(&self, program: &Program, sizes: &[usize], clean: bool) {
        for &size in sizes {
            let r = self.verify(program, size);
            assert_eq!(r.mismatches, 0, "{} @{size}: {:#?}", self.workload.name, r.samples);
            assert_eq!(r.faults, r.interp, "{} @{size}: typed faults {} vs interp {}: {:#?}", self.workload.name, r.faults, r.interp, r.samples);
            assert_eq!(r.spurious, 0, "{} @{size}: typed faults on rows the VM evaluates", self.workload.name);
            if clean {
                assert_eq!(r.faults, 0, "{} @{size}", self.workload.name);
            }
        }
    }
}

struct Bench;

impl Bench {
    fn rows(name: &str, size: usize, reps: usize) -> f64 {
        let mut workload = Workload::get(name);
        for c in workload.spec.columns.iter_mut().chain(workload.spec.lists.iter_mut().flat_map(|l| l.fields.iter_mut())) {
            if c.enc == Enc::Dec {
                c.enc = Enc::Scaled;
            }
        }
        let hir = workload.hir().expect("lowering");
        let rows = workload.rows(size);
        let frame = Frame::new(&hir, &workload.spec, &rows, 0);
        let batch = frame.batch();
        let program = Program::new(hir).expect("program");
        let mut out = program.outputs();
        program.run(&batch, &mut out, None).expect("run");
        let mut best = f64::MAX;
        for _ in 0..reps {
            let t = Instant::now();
            for _ in 0..50 {
                program.run(&batch, &mut out, None).expect("run");
            }
            best = best.min(t.elapsed().as_nanos() as f64 / 50.0 / size as f64);
        }
        best
    }
}

const SIZES: [usize; 5] = [1024, 64, 16, 7, 1];

#[test]
fn workloads_match_vm_and_interp() {
    for name in Workload::NAMES {
        let workload = Workload::get(name);
        let rows = workload.rows(1024);
        let lab = Lab::new(workload, rows);
        let program = Program::new(lab.hir.clone()).expect("program");
        lab.check(&program, &SIZES, !Workload::FAULTY.contains(&name));
    }
}

#[test]
fn extremes_match_vm_and_interp() {
    for name in Workload::NAMES {
        let workload = Workload::get(name);
        let rows = workload.mixed(300);
        let lab = Lab::new(workload, rows);
        let program = Program::new(lab.hir.clone()).expect("program");
        lab.check(&program, &SIZES, false);
    }
}

#[test]
fn passes_off_match_vm() {
    let offs: [fn(&mut Passes); 7] = [
        |p| p.fold = false,
        |p| p.factor = false,
        |p| p.skip = false,
        |p| p.reuse = false,
        |p| p.elide = false,
        |p| *p = Passes { fold: false, factor: false, skip: false, reuse: false, elide: false, skip_min: 2, lookup_min: 128 },
        |p| p.lookup_min = 1,
    ];
    for name in Workload::NAMES {
        let workload = Workload::get(name);
        let rows = workload.mixed(200);
        let lab = Lab::new(workload, rows);
        for off in offs {
            let mut passes = Passes::ALL;
            off(&mut passes);
            let program = Program::with(lab.hir.clone(), passes).expect("program");
            lab.check(&program, &[1024, 7], false);
        }
    }
}

#[test]
fn untaken_arm_faults_are_masked() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let huge = [i64::MAX / 3, -(i64::MAX / 7), 4_000_000_000_000_000_000, 99_999_999_999, 1];
    for name in Workload::NAMES {
        let workload = Workload::get(name);
        let mut rows = workload.rows(600);
        let mut bump = |v: &mut Value| {
            if let Value::Num(d) = v {
                match next() % 8 {
                    0 | 1 => *v = Value::Num(Dec::new(huge[(next() % 5) as usize], d.scale)),
                    2 => *v = Value::Null,
                    _ => {}
                }
            }
        };
        for row in rows.iter_mut() {
            row.columns.iter_mut().for_each(&mut bump);
            row.lists.iter_mut().flatten().flatten().for_each(&mut bump);
        }
        let lab = Lab::new(workload, rows);
        let program = Program::new(lab.hir.clone()).expect("program");
        for size in [1024, 64, 7, 1] {
            let r = lab.verify(&program, size);
            assert_eq!(r.mismatches, 0, "{name} @{size}: {:#?}", r.samples);
            assert_eq!(r.faults, r.interp, "{name} @{size}: {:#?}", r.samples);
            assert!(r.faults > 0, "{name} @{size}");
        }
    }
}

#[test]
#[ignore]
fn micro_bench() {
    for name in Workload::NAMES {
        let ns = Bench::rows(name, 1024, 15);
        println!("{name} @1024: {ns:.1} ns/row");
    }
}

#[test]
#[ignore]
fn profile_ops() {
    use crate::typed::exec::Exec;
    for name in Workload::NAMES {
        Exec::PROFILE.with_borrow_mut(|p| *p = Some(Default::default()));
        Bench::rows(name, 1024, 5);
        let mut ops: Vec<(String, (u128, usize))> = Exec::PROFILE.with_borrow_mut(|p| p.take()).unwrap_or_default().into_iter().collect();
        ops.sort_by_key(|o| std::cmp::Reverse(o.1 .0));
        let runs = 5.0 * 50.0 + 1.0;
        println!("{name}");
        for (op, (ns, count)) in ops {
            println!("  {op:<12} {:>7.2} ns/row {:>6.1} per run", ns as f64 / runs / 1024.0, count as f64 / runs);
        }
    }
}

#[test]
#[ignore]
fn dump_bytecode() {
    for name in Workload::NAMES {
        let workload = Workload::get(name);
        let hir = workload.hir().expect("lowering");
        let rows = workload.rows(1024);
        let frame = Frame::new(&hir, &workload.spec, &rows, 0);
        let program = Program::new(hir.clone()).expect("program");
        println!("== {name}\n{hir}\n{}", program.dump(&frame.batch()).expect("dump"));
    }
}

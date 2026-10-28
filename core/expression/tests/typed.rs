mod conformance;

use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::sync::Arc;
use zen_expression::lane::{Column, Fusion, Values};
use zen_expression::typed::{
    Batch, Class, Data, Hir, List, Lowering, Program, Resolve, Resolved, Slot,
};
use zen_expression::{ExpressionKind, Variable};

#[derive(Clone, Copy, PartialEq)]
enum Seen {
    Absent,
    Class(Class),
    List,
    Mixed,
}

struct Probe<'c> {
    rows: &'c [Variable],
}

enum Store {
    Num(Vec<Decimal>),
    Text { offsets: Vec<i32>, data: String },
    Bool(Vec<u64>),
}

struct Owned {
    store: Store,
    valid: Vec<u64>,
}

struct OwnedList {
    offsets: Vec<i32>,
    valid: Vec<u64>,
    fields: Vec<Owned>,
}

#[derive(Default)]
struct Tally {
    rows: usize,
    lowered: usize,
    typed: usize,
}

#[derive(Default)]
struct Report {
    files: BTreeMap<String, Tally>,
    spurious: Vec<String>,
    mismatches: Vec<String>,
    reasons: BTreeMap<String, usize>,
}

impl Seen {
    fn of(value: &Variable) -> Seen {
        match value {
            Variable::Null => Seen::Absent,
            Variable::Number(_) => Seen::Class(Class::Num),
            Variable::String(_) => Seen::Class(Class::Text),
            Variable::Bool(_) => Seen::Class(Class::Bool),
            Variable::Array(_) => Seen::List,
            _ => Seen::Mixed,
        }
    }

    fn join(self, other: Seen) -> Seen {
        match (self, other) {
            (Seen::Absent, x) | (x, Seen::Absent) => x,
            (x, y) if x == y => x,
            _ => Seen::Mixed,
        }
    }

    fn all<'v>(values: impl Iterator<Item = &'v Variable>) -> Seen {
        values.fold(Seen::Absent, |acc, v| acc.join(Seen::of(v)))
    }
}

impl Probe<'_> {
    fn values(&self, path: &str) -> Vec<Variable> {
        self.rows
            .iter()
            .map(|row| row.dot(path).unwrap_or(Variable::Null))
            .collect()
    }

    fn elements(values: &[Variable]) -> Vec<Variable> {
        values
            .iter()
            .filter_map(|v| match v {
                Variable::Array(items) => Some(items.borrow().clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn fields(elements: &[Variable]) -> Option<Vec<(Arc<str>, Class)>> {
        if elements.iter().any(|e| matches!(e, Variable::Null)) {
            return None;
        }
        if !elements.iter().all(|e| matches!(e, Variable::Object(_))) {
            return match Seen::all(elements.iter()) {
                Seen::Class(class) => Some(vec![(Arc::from(""), class)]),
                Seen::Absent => Some(Vec::new()),
                _ => None,
            };
        }
        let mut keys: Vec<String> = Vec::new();
        for e in elements {
            if let Variable::Object(o) = e {
                for (k, _) in o.borrow().iter() {
                    if !keys.iter().any(|x| x.as_str() == k.as_str()) {
                        keys.push(k.to_string());
                    }
                }
            }
        }
        let mut fields = Vec::new();
        for key in keys {
            let values: Vec<Variable> = elements
                .iter()
                .map(|e| e.dot(&key).unwrap_or(Variable::Null))
                .collect();
            if let Seen::Class(class) = Seen::all(values.iter()) {
                fields.push((Arc::from(key.as_str()), class));
            }
        }
        Some(fields)
    }
}

impl Resolve for Probe<'_> {
    fn path(&mut self, path: &str) -> Resolved {
        let values = self.values(path);
        match Seen::all(values.iter()) {
            Seen::Absent => Resolved::Absent,
            Seen::Class(class) => Resolved::Column(class),
            Seen::List => match Self::fields(&Self::elements(&values)) {
                Some(fields) => Resolved::List { fields },
                None => Resolved::Unsupported,
            },
            Seen::Mixed => Resolved::Unsupported,
        }
    }

    fn alias(&mut self, _: &str) -> Resolved {
        Resolved::Unsupported
    }
}

impl Owned {
    fn build(class: Class, values: &[Variable]) -> Owned {
        let mut valid = vec![0u64; values.len().div_ceil(64).max(1)];
        for (i, v) in values.iter().enumerate() {
            if !matches!(v, Variable::Null) {
                valid[i / 64] |= 1 << (i % 64);
            }
        }
        let store = match class {
            Class::Num => Store::Num(
                values
                    .iter()
                    .map(|v| v.as_number().unwrap_or_default())
                    .collect(),
            ),
            Class::Bool => {
                let mut bits = vec![0u64; values.len().div_ceil(64).max(1)];
                for (i, v) in values.iter().enumerate() {
                    if matches!(v, Variable::Bool(true)) {
                        bits[i / 64] |= 1 << (i % 64);
                    }
                }
                Store::Bool(bits)
            }
            _ => {
                let mut offsets = vec![0i32];
                let mut data = String::new();
                for v in values {
                    if let Variable::String(s) = v {
                        data.push_str(s);
                    }
                    offsets.push(data.len() as i32);
                }
                Store::Text { offsets, data }
            }
        };
        Owned { store, valid }
    }

    fn column(&self) -> Column<'_> {
        let values = match &self.store {
            Store::Num(n) => Values::Dec(n),
            Store::Text { offsets, data } => Values::Text { offsets, data },
            Store::Bool(bits) => Values::Bool { bits, offset: 0 },
        };
        Column::with_validity(values, &self.valid, 0)
    }
}

impl Report {
    const DATES: [&'static str; 4] = [
        "standard/08-",
        "standard/10-05",
        "standard/16-",
        "standard/18-",
    ];

    fn signature(value: &Variable) -> String {
        match value {
            Variable::Object(o) => {
                let mut keys: Vec<String> = o
                    .borrow()
                    .iter()
                    .map(|(k, v)| format!("{k}:{}", Self::signature(v)))
                    .collect();
                keys.sort();
                format!("{{{}}}", keys.join(","))
            }
            Variable::Array(items) => format!(
                "[{}]",
                items
                    .borrow()
                    .iter()
                    .map(Self::signature)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            other => other.type_name().to_string(),
        }
    }

    fn output(slot: &Slot, hir: &Hir, at: usize) -> Variable {
        match &slot.data {
            Data::Num { .. } => slot.decimal(at).map_or(Variable::Null, Variable::Number),
            Data::Bool(_) => slot.boolean(at).map_or(Variable::Null, Variable::Bool),
            Data::Code(_) => match (slot.code(at), slot.dict()) {
                (Some(c), Some(d)) => hir
                    .dict(d)
                    .text(c)
                    .map_or(Variable::Null, |t| Variable::String(t.into())),
                _ => Variable::Null,
            },
            Data::Text { .. } => slot
                .text(at)
                .map_or(Variable::Null, |t| Variable::String(t.into())),
            Data::Date(_) => slot.variable(at),
        }
    }

    fn same(a: &Variable, b: &Variable) -> bool {
        a.type_name() == b.type_name()
            && conformance::Spec::render(a) == conformance::Spec::render(b)
    }

    fn group(&mut self, expression: &str, cases: &[&conformance::SpecCase]) {
        let rows: Vec<Variable> = cases
            .iter()
            .map(|c| c.input.clone().unwrap_or(Variable::Null))
            .collect();
        for case in cases {
            self.files.entry(case.file.clone()).or_default().rows += 1;
        }
        let mut probe = Probe { rows: &rows };
        let mut lowering = Lowering::new(None);
        let entry = Fusion::Output {
            key: "zz__out".to_string(),
            source: expression.to_string(),
        };
        if let Err(e) = lowering.unit(&[entry], &mut probe) {
            if cases
                .iter()
                .any(|c| Self::DATES.iter().any(|d| c.file.contains(d)))
            {
                let reason: String = e
                    .to_string()
                    .chars()
                    .map(|c| if c.is_ascii_digit() { 'N' } else { c })
                    .collect();
                *self.reasons.entry(reason).or_default() += cases.len();
            }
            return;
        }
        let Ok(program) = Program::new(lowering.finish()) else {
            return;
        };
        let hir = program.hir();
        let columns: Vec<Owned> = hir
            .schema
            .columns
            .iter()
            .map(|c| Owned::build(Self::class(c.ty.kind), &probe.values(&c.path)))
            .collect();
        let mut lists = Vec::new();
        for def in &hir.schema.lists {
            if def.parent.is_some() {
                return;
            }
            let values = probe.values(&def.path);
            let mut offsets = vec![0i32];
            let mut valid = vec![0u64; rows.len().div_ceil(64).max(1)];
            let mut elements = Vec::new();
            for (r, v) in values.iter().enumerate() {
                if let Variable::Array(items) = v {
                    valid[r / 64] |= 1 << (r % 64);
                    elements.extend(items.borrow().iter().cloned());
                }
                offsets.push(elements.len() as i32);
            }
            let fields = def
                .fields
                .iter()
                .map(|f| {
                    let values: Vec<Variable> = elements
                        .iter()
                        .map(|e| match f.path.is_empty() {
                            true => e.clone(),
                            false => e.dot(&f.path).unwrap_or(Variable::Null),
                        })
                        .collect();
                    Owned::build(Self::class(f.ty.kind), &values)
                })
                .collect();
            lists.push(OwnedList {
                offsets,
                valid,
                fields,
            });
        }
        let mut batch = Batch::new(rows.len());
        for c in &columns {
            batch.push_column(c.column());
        }
        for l in &lists {
            batch.push_list(List::new(
                &l.offsets,
                Some((&l.valid, 0)),
                l.fields.iter().map(Owned::column).collect(),
            ));
        }
        let mut out = program.outputs();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            program.run(&batch, &mut out, None)
        }));
        match ran {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return,
            Err(_) => {
                self.mismatches.push(format!("PANIC {expression}"));
                return;
            }
        }
        let Some(slot) = out.slot(0) else {
            return;
        };
        for (row, case) in cases.iter().enumerate() {
            let tally = self.files.entry(case.file.clone()).or_default();
            tally.lowered += 1;
            let want = case.stack();
            if out.faults().get(row) {
                if want.is_ok() {
                    self.spurious.push(format!(
                        "{} | {expression} | input {}",
                        case.location(),
                        case.input_text
                    ));
                }
                continue;
            }
            tally.typed += 1;
            let got = match &slot.offsets {
                Some(offsets) => {
                    let (lo, hi) = (offsets[row] as usize, offsets[row + 1] as usize);
                    Variable::from_array((lo..hi).map(|at| Self::output(slot, hir, at)).collect())
                }
                None => Self::output(slot, hir, row),
            };
            let rendered = conformance::Spec::guard(|| {
                Ok(Variable::String(conformance::Spec::render(&got).into()))
            });
            let fine = match (&want, &rendered) {
                (Ok(v), Ok(_)) => Self::same(v, &got),
                (Err(e), Err(r)) => {
                    e.starts_with(conformance::Spec::PANIC)
                        && r.starts_with(conformance::Spec::PANIC)
                }
                _ => false,
            };
            if !fine {
                self.mismatches.push(format!(
                    "{} | {expression} | input {} | vm {} | typed {}",
                    case.location(),
                    case.input_text,
                    want.as_ref()
                        .map_or_else(|e| format!("!error ({e})"), conformance::Spec::render),
                    rendered.map_or_else(|e| e, |r| r.as_str().unwrap_or_default().to_string())
                ));
            }
        }
    }

    fn class(kind: zen_expression::typed::Kind) -> Class {
        match kind {
            zen_expression::typed::Kind::Num(_) => Class::Num,
            zen_expression::typed::Kind::Bool => Class::Bool,
            _ => Class::Text,
        }
    }

    fn print(&self) {
        let mut all = Tally::default();
        let mut dates = Tally::default();
        for (file, t) in &self.files {
            for acc in [&mut all].into_iter().chain(
                Self::DATES
                    .iter()
                    .any(|d| file.contains(d))
                    .then_some(&mut dates),
            ) {
                acc.rows += t.rows;
                acc.lowered += t.lowered;
                acc.typed += t.typed;
            }
            eprintln!(
                "typed spec {file}: {} rows, {} lowered, {} typed",
                t.rows, t.lowered, t.typed
            );
        }
        eprintln!(
            "typed spec all: {} rows, {} lowered, {} typed",
            all.rows, all.lowered, all.typed
        );
        eprintln!(
            "typed spec dates: {} rows, {} lowered, {} typed",
            dates.rows, dates.lowered, dates.typed
        );
        let mut reasons: Vec<(&String, &usize)> = self.reasons.iter().collect();
        reasons.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (reason, n) in reasons.iter().take(40) {
            eprintln!("typed spec date rows not lowered: {n} {reason}");
        }
        eprintln!("typed spec spurious faults: {}", self.spurious.len());
        for s in self.spurious.iter().take(20) {
            eprintln!("  spurious {s}");
        }
    }
}

#[test]
fn typed_matches_spec() {
    conformance::Spec::prepare();
    let (cases, problems) = conformance::Spec::cases();
    assert!(
        problems.is_empty(),
        "malformed spec:\n{}",
        problems.join("\n")
    );
    let mut groups: BTreeMap<(String, String), Vec<&conformance::SpecCase>> = BTreeMap::new();
    let rooted = |c: &&conformance::SpecCase| matches!(c.input, None | Some(Variable::Object(_)));
    for case in cases
        .iter()
        .filter(|c| matches!(c.kind, ExpressionKind::Standard))
        .filter(rooted)
    {
        groups
            .entry((
                case.expression.clone(),
                Report::signature(&case.input.clone().unwrap_or(Variable::Null)),
            ))
            .or_default()
            .push(case);
    }
    let mut report = Report::default();
    for ((expression, _), group) in &groups {
        report.group(expression, group);
    }
    report.print();
    assert!(
        report.mismatches.is_empty(),
        "{} typed/stack differences:\n{}",
        report.mismatches.len(),
        report
            .mismatches
            .iter()
            .take(60)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[path = "support/policy_gen.rs"]
mod policy_gen;
#[allow(dead_code)]
mod conformance;
mod support;

use policy_gen::{Gen, Rng};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use zen_engine::policy::{EvaluateRequest, EvaluationError, EvaluationResult, PolicyWorkspace};
use zen_expression::Variable;

const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/policy/fixtures/");
const EVALUATION: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/policy/evaluation.toml");

#[derive(Deserialize)]
struct EvaluationFile {
    test: Vec<EvaluationCase>,
}

#[derive(Deserialize)]
struct EvaluationCase {
    name: String,
    policies: Vec<String>,
    input: toml::Value,
}

struct Differential {
    compared: usize,
    succeeded: usize,
    failures: Vec<String>,
    kinds: std::collections::BTreeMap<String, usize>,
}

impl Differential {
    fn shape(result: &Result<EvaluationResult, EvaluationError>) -> Result<Value, String> {
        match result {
            Ok(result) => Ok(result.output.clone().into()),
            Err(error) => Err(format!("{error:?}")),
        }
    }

    fn request(path: &str, input: &Value) -> EvaluateRequest {
        Self::with_goals(path, input, &[])
    }

    fn with_goals(path: &str, input: &Value, goals: &[&str]) -> EvaluateRequest {
        EvaluateRequest {
            policy_path: Arc::from(path),
            input: Variable::from(input.clone()),
            goals: goals.iter().map(|g| Arc::from(*g)).collect(),
            trace: false,
        }
    }

    fn leaves(value: &Value, prefix: &str, out: &mut Vec<String>) {
        let Value::Object(map) = value else {
            out.push(prefix.to_string());
            return;
        };
        for (key, child) in map {
            let path = match prefix.is_empty() {
                true => key.clone(),
                false => format!("{prefix}.{key}"),
            };
            Self::leaves(child, &path, out);
        }
    }

    fn computed(ws: &PolicyWorkspace, path: &str, inputs: &[Value]) -> Vec<String> {
        let mut out = Vec::new();
        for input in inputs {
            let Ok(result) = ws.evaluate_with_driver(&Self::request(path, input)) else {
                continue;
            };
            let (mut produced, mut given) = (Vec::new(), Vec::new());
            Self::leaves(&result.output.into(), "", &mut produced);
            Self::leaves(input, "", &mut given);
            out = produced.into_iter().filter(|p| !given.contains(p)).collect();
            break;
        }
        out
    }

    fn run(&mut self, label: &str, ws: &PolicyWorkspace, path: &str, inputs: &[Value]) {
        let computed = Self::computed(ws, path, inputs);
        let choices: Vec<Vec<&str>> = std::iter::once(Vec::new())
            .chain(computed.iter().map(|c| vec![c.as_str()]))
            .chain(computed.windows(2).map(|w| vec![w[0].as_str(), w[1].as_str()]))
            .collect();
        let goals = |i: usize| choices[(i * 7) % choices.len()].as_slice();
        let requests: Vec<EvaluateRequest> = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| Self::with_goals(path, input, goals(i)))
            .collect();
        let batch = ws.evaluate_batch(&requests);
        for (i, (input, batched)) in inputs.iter().zip(&batch).enumerate() {
            let want = Self::shape(&ws.evaluate_with_driver(&Self::with_goals(path, input, goals(i))));
            let single = Self::shape(&ws.evaluate(&Self::with_goals(path, input, goals(i))));
            let batched = Self::shape(batched);
            self.compared += 1;
            self.succeeded += usize::from(want.is_ok());
            if let Err(error) = &want {
                let kind: String = error.chars().take_while(|c| c.is_alphanumeric()).collect();
                *self.kinds.entry(kind).or_default() += 1;
            }
            if single != want {
                self.failures.push(format!("{label} single {input}\n  driver   {want:?}\n  compiled {single:?}"));
            }
            if batched != want {
                self.failures.push(format!("{label} batch {input}\n  driver   {want:?}\n  compiled {batched:?}"));
            }
        }
    }

    fn variants(base: &Value) -> Vec<Value> {
        let mut out = vec![base.clone()];
        let Some(object) = base.as_object() else {
            return out;
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
            with(Some(Value::Null));
            if let Value::Object(inner) = value {
                for (field, v) in inner {
                    let mut nested = inner.clone();
                    nested.insert(
                        field.clone(),
                        match v {
                            Value::Number(_) => json!("text"),
                            Value::String(_) => json!(7),
                            Value::Bool(b) => json!(!b),
                            Value::Array(_) => json!([]),
                            _ => Value::Null,
                        },
                    );
                    with(Some(Value::Object(nested)));
                }
            }
        }
        out
    }

    fn finish(self) {
        println!("compared {} inputs, {} evaluated without error, errors {:?}", self.compared, self.succeeded, self.kinds);
        assert!(
            self.failures.is_empty(),
            "{} mismatches:\n{}",
            self.failures.len(),
            self.failures.iter().take(15).cloned().collect::<Vec<_>>().join("\n")
        );
    }
}

#[test]
fn compiled_policies_match_the_driver_on_fixtures() {
    let raw = std::fs::read_to_string(EVALUATION).expect("evaluation.toml");
    let file: EvaluationFile = toml::from_str(&raw).expect("evaluation cases");
    let mut differential = Differential {
        compared: 0,
        succeeded: 0,
        failures: Vec::new(),
        kinds: Default::default(),
    };
    for case in &file.test {
        let mut ws = PolicyWorkspace::new();
        for path in &case.policies {
            let text = std::fs::read_to_string(format!("{FIXTURES_DIR}{path}")).expect("fixture");
            ws.set_policy(path.as_str(), serde_json::from_str(&text).expect("policy document"));
        }
        let input: Value = serde_json::to_value(&case.input).expect("input");
        differential.run(&case.name, &ws, &case.policies[0], &Differential::variants(&input));
    }
    differential.finish();
}

#[test]
fn compiled_policies_match_the_driver_on_generated_documents() {
    let seeds: u64 = std::env::var("POLICY_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let mut differential = Differential {
        compared: 0,
        succeeded: 0,
        failures: Vec::new(),
        kinds: Default::default(),
    };
    for seed in 0..seeds {
        let document = Gen::document(seed);
        let mut ws = PolicyWorkspace::new();
        let Ok(parsed) = serde_json::from_value(document) else {
            continue;
        };
        ws.set_policy("generated.json", parsed);
        let mut rng = Rng(seed.wrapping_mul(7919) | 1);
        let inputs: Vec<Value> = (0..24)
            .map(|i| match i % 3 {
                0 => Gen::input(&mut rng),
                _ => Gen::valid_input(&mut rng),
            })
            .collect();
        differential.run(&format!("seed {seed}"), &ws, "generated.json", &inputs);
        Columnar::check(&format!("seed {seed}"), &ws, &inputs, &mut differential.failures);
    }
    differential.finish();
}

struct Columnar;

impl Columnar {
    fn check(label: &str, ws: &PolicyWorkspace, inputs: &[Value], failures: &mut Vec<String>) {
        for built in Built::new(inputs).into_iter().chain(Built::lists(inputs)) {
            Self::compare(label, ws, &built.columns(), failures);
        }
        if let Some(built) = Built::nested(inputs) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            Self::compare(label, ws, &built.columns_with(&structs), failures);
        }
    }

    fn compare(label: &str, ws: &PolicyWorkspace, columns: &zen_expression::lane::Columns, failures: &mut Vec<String>) {
        let Ok(output) = ws.evaluate_columns(&Arc::from("generated.json"), &[], columns) else {
            return;
        };
        for row in 0..columns.rows {
            let input = columns.row(row);
            let request = EvaluateRequest {
                policy_path: Arc::from("generated.json"),
                input: input.depth_clone(64),
                goals: Vec::new(),
                trace: false,
            };
            let want = ws
                .evaluate_with_driver(&request)
                .map(|r| Built::normalized(r.output.to_value()))
                .map_err(|e| format!("{e:?}"));
            let got = match &output.errors[row] {
                Some(error) => Err(format!("{error:?}")),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            if got != want {
                failures.push(format!("{label} columnar row {row} input {}\n  driver   {want:?}\n  columnar {got:?}", input.to_value()));
            }
        }
    }
}

#[test]
#[ignore]
fn compiled_policy_throughput() {
    let rows: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let seeds: u64 = std::env::var("POLICY_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let time = |elapsed: std::time::Duration| elapsed.as_nanos() as f64 / rows as f64;
    let mut ratios: Vec<(f64, f64, f64)> = Vec::new();
    for seed in 0..seeds {
        let mut ws = PolicyWorkspace::new();
        let Ok(parsed) = serde_json::from_value(Gen::document(seed)) else {
            continue;
        };
        ws.set_policy("generated.json", parsed);
        let mut rng = Rng(seed.wrapping_mul(7919) | 1);
        let requests: Vec<EvaluateRequest> = (0..rows)
            .map(|_| Differential::request("generated.json", &Gen::valid_input(&mut rng)))
            .collect();
        if let Ok(profile) = std::env::var("BENCH_PROFILE") {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(profile.parse().unwrap_or(1));
            while std::time::Instant::now() < until {
                match std::env::var("BENCH_MODE").as_deref() {
                    Ok("single") => requests.iter().for_each(|r| drop(ws.evaluate(r))),
                    Ok("columns") => {
                        if let Some(columns) = Built::new(&requests.iter().map(|r| r.input.to_value()).collect::<Vec<_>>()).as_ref().map(Built::columns) {
                            drop(ws.evaluate_columns(&Arc::from("generated.json"), &[], &columns));
                        }
                    }
                    _ => drop(ws.evaluate_batch(&requests)),
                }
            }
            continue;
        }
        let values: Vec<Value> = requests.iter().map(|r| r.input.to_value()).collect();
        let built = Built::new(&values);
        let mut best = [f64::MAX; 4];
        let fresh = || -> Vec<EvaluateRequest> {
            requests
                .iter()
                .map(|r| EvaluateRequest {
                    input: r.input.depth_clone(usize::MAX),
                    ..r.clone()
                })
                .collect()
        };
        for _ in 0..5 {
            let batch = fresh();
            let start = std::time::Instant::now();
            batch.iter().for_each(|r| drop(ws.evaluate_with_driver(r)));
            best[0] = best[0].min(time(start.elapsed()));
            let batch = fresh();
            let start = std::time::Instant::now();
            batch.iter().for_each(|r| drop(ws.evaluate(r)));
            best[1] = best[1].min(time(start.elapsed()));
            let batch = fresh();
            let start = std::time::Instant::now();
            drop(ws.evaluate_batch(&batch));
            best[2] = best[2].min(time(start.elapsed()));
            if let Some(columns) = built.as_ref().map(Built::columns) {
                let start = std::time::Instant::now();
                drop(ws.evaluate_columns(&Arc::from("generated.json"), &[], &columns));
                best[3] = best[3].min(time(start.elapsed()));
            }
        }
        println!("seed {seed:3} driver {:>8.0} single {:>8.0} batch {:>8.0} columns {:>8.0}  {:.2}x {:.2}x {:.2}x", best[0], best[1], best[2], best[3], best[0] / best[1], best[0] / best[2], best[0] / best[3]);
        ratios.push((best[0] / best[1], best[0] / best[2], best[0] / best[3]));
    }
    let gm = |f: fn(&(f64, f64, f64)) -> f64| (ratios.iter().map(|r| f(r).ln()).sum::<f64>() / ratios.len() as f64).exp();
    let mut columnar: Vec<f64> = ratios.iter().map(|r| r.2).collect();
    columnar.sort_by(f64::total_cmp);
    println!("single: geomean {:.2}x | batch: geomean {:.2}x | columns: geomean {:.2}x median {:.2}x", gm(|r| r.0), gm(|r| r.1), gm(|r| r.2), columnar.get(columnar.len() / 2).copied().unwrap_or_default());
}

#[tokio::test]
async fn engine_batch_matches_single_evaluations() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{DecisionEngine, EvaluationOptions};

    let loader = Arc::new(MemoryLoader::default());
    let mut rng = Rng(11);
    let document = serde_json::from_value(Gen::document(3)).expect("policy document");
    loader.add("policy", DecisionContent::Policy(PolicyContent(Arc::new(document))));
    let graph: DecisionContent = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/graphs/loan-approval.json"))
            .or_else(|_| std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/loan-approval.json")))
            .expect("graph fixture"),
    )
    .expect("graph content");
    loader.add("graph", graph);
    let contexts: Vec<Variable> = (0..40)
        .map(|i| match i % 2 {
            0 => Variable::from(Gen::valid_input(&mut rng)),
            _ => Variable::from(Gen::input(&mut rng)),
        })
        .collect();
    let engine = DecisionEngine::default().with_loader(loader);
    for compiled in [false, true] {
        if compiled {
            engine.compile();
        }
        for key in ["policy", "graph"] {
            let fresh: Vec<Variable> = contexts.iter().map(Variable::deep_clone).collect();
            let batch = engine.evaluate_batch(key, &fresh, EvaluationOptions::default()).await;
            assert_eq!(batch.len(), contexts.len());
            for (context, batched) in contexts.iter().zip(batch) {
                let single = engine
                    .evaluate_with_opts(key, context.deep_clone(), EvaluationOptions::default())
                    .await;
                let shape = |r: Result<zen_engine::DecisionGraphResponse, Box<zen_engine::EvaluationError>>| {
                    r.map(|r| r.result.to_value()).map_err(|e| e.to_string())
                };
                assert_eq!(shape(batched), shape(single), "{key} compiled={compiled} {context:?}");
            }
        }
    }
}

struct Anchor;

impl Anchor {
    const POLICY: &'static str = r#"{
  models: {
    customer: {age: "number", income: "number", tier: "string", rate: "number", accounts: "account[]"},
    account: {balance: "number", kind: "string", active: "boolean"},
  },
  blocks: [
    {expression: "account.fee", value: "account.balance * customer.rate / 100 * (account.kind == 'loan' ? 2 : 1)"},
    {match: "account.label", arms: [["account.kind == 'savings'", "'S'"], ["account.kind == 'checking'", "'C'"], ["", "'L'"]]},
    {expression: "customer.total", value: "sum(map(filter(customer.accounts, #.active), #.balance))"},
    {expression: "customer.fees", value: "sum(map(customer.accounts, #.fee))"},
    {match: "customer.risk", arms: [["customer.total > customer.income * 2", "'high'"], ["customer.total > customer.income", "'mid'"], ["", "'low'"]]},
    {table: "discount", inputs: ["customer.tier", "customer.risk"], outputs: ["customer.discount"], rules: [
      ["'gold'", "'low'", "0.15"],
      ["'gold'", "'mid'", "0.1"],
      ["'silver'", "'low'", "0.1"],
      ["'silver'", "'mid'", "0.05"],
      ["", "'low'", "0.02"],
      ["", "", "0"],
    ]},
    {expression: "customer.net", value: "customer.total - customer.fees - customer.discount"},
    {assertion: "customer.eligible", conditions: ["customer.age >= 18", "customer.risk != 'high'"]},
  ],
}"#;

    const PROGRAMS: [(&'static str, &'static str); 8] = [
        ("fee", "account.balance * customer.rate / 100 * (account.kind == 'loan' ? 2 : 1)"),
        ("label", "account.kind == 'savings' ? 'S' : (account.kind == 'checking' ? 'C' : 'L')"),
        ("total", "sum(map(filter(customer.accounts, #.active), #.balance))"),
        ("fees", "sum(map(customer.accounts, #.fee))"),
        ("risk", "customer.total > customer.income * 2 ? 'high' : (customer.total > customer.income ? 'mid' : 'low')"),
        ("discount", "customer.tier == 'gold' and customer.risk == 'low' ? 0.15 : (customer.tier == 'gold' and customer.risk == 'mid' ? 0.1 : (customer.tier == 'silver' and customer.risk == 'low' ? 0.1 : (customer.tier == 'silver' and customer.risk == 'mid' ? 0.05 : (customer.risk == 'low' ? 0.02 : 0))))"),
        ("net", "customer.total - customer.fees - customer.discount"),
        ("eligible", "customer.age >= 18 and customer.risk != 'high'"),
    ];

    const KEYS: [&'static str; 8] = [
        "account.fee",
        "account.label",
        "customer.total",
        "customer.fees",
        "customer.risk",
        "customer.discount",
        "customer.net",
        "customer.eligible",
    ];

    fn fused(columns: &zen_expression::lane::Columns, programs: &mut Vec<Option<zen_expression::lane::LaneProgram>>, runner: &mut zen_expression::lane::LaneRunner, stages: &mut [f64; 2]) -> Option<Vec<Value>> {
        use zen_expression::lane::{Column, Columns, Dictionary, LaneProgram, Output, Values};
        let rows = columns.rows;
        let find = |path: &str| columns.find(path).map(|i| columns.columns[i].1);
        let (accounts, rate) = (find("customer.accounts")?, find("customer.rate")?);
        let Values::List { offsets, child: Dictionary::Column(record) } = accounts.values else {
            return None;
        };
        let parent: Vec<usize> = (0..rows).flat_map(|row| {
            let (a, b) = (offsets[row] as usize, offsets[row + 1] as usize);
            std::iter::repeat_n(row, b - a)
        }).collect();
        let gathered: Vec<rust_decimal::Decimal> = parent.iter().map(|p| rate.number(*p).unwrap_or_default()).collect();
        let field = |name: &str| record.field(name).copied();
        let (balance, kind, active) = (field("balance")?, field("kind")?, field("active")?);
        let child = Columns::new(parent.len())
            .column("account.balance", balance)
            .column("account.kind", kind)
            .column("account.active", active)
            .column("customer.rate", Column::new(Values::Dec(&gathered)));
        let mut run = |index: usize, range: std::ops::Range<usize>, columns: &Columns, outs: &mut Vec<Output>, stages: &mut [f64; 2]| {
            let start = std::time::Instant::now();
            let program = programs[index].get_or_insert_with(|| {
                let entries: Vec<zen_expression::lane::Fusion> = range
                    .clone()
                    .map(|i| zen_expression::lane::Fusion::Output {
                        key: Self::KEYS[i].to_string(),
                        source: std::env::var(format!("ANCHOR_SOURCE_{i}")).unwrap_or_else(|_| Self::PROGRAMS[i].1.to_string()),
                    })
                    .collect();
                let generic = LaneProgram::compile_fused(&entries).expect("fused program");
                let program = generic.specialize_columns(columns).unwrap_or(generic);
                if std::env::var("ANCHOR_DUMP").is_ok() {
                    eprintln!("== fused {index}\n{:#?}", program.program().steps);
                }
                program
            });
            runner.evaluate_columns_many(program, columns, outs, |_, _, _| {});
            stages[index] += start.elapsed().as_nanos() as f64 / rows as f64;
        };
        let mut member: Vec<Output> = (0..2).map(|_| Output::new()).collect();
        member[1].prefer_codes();
        run(0, 0..2, &child, &mut member, stages);
        let [fee, label] = &member[..] else {
            return None;
        };
        let label_offsets: Vec<i32> = label.offsets().iter().map(|o| *o as i32).collect();
        let label_column = match label.codes() {
            Some((keys, offsets, data)) => Column::new(Values::Dict { keys, values: Dictionary::Text { offsets, data } }),
            None => Column::new(Values::Utf8 { offsets: &label_offsets, data: label.data() }),
        };
        let fields = [
            ("balance", balance),
            ("kind", kind),
            ("active", active),
            ("fee", Column::new(Values::Scaled { mant: fee.mantissas(), scale: fee.scales() })),
            ("label", label_column),
        ];
        let composed = Column::new(Values::Struct { fields: &fields, len: parent.len() });
        let list = Column::new(Values::List { offsets, child: Dictionary::Column(&composed) });
        let mut root = Columns::new(rows).column("customer.accounts", list);
        for path in ["customer.age", "customer.income", "customer.tier"] {
            if let Some(column) = find(path) {
                root = root.column(path, column);
            }
        }
        let mut outs: Vec<Output> = (0..6).map(|_| Output::new()).collect();
        outs[2].prefer_codes();
        run(1, 2..8, &root, &mut outs, stages);
        let check = std::env::var("ANCHOR_CHECK").is_ok();
        check.then(|| {
            (0..rows)
                .map(|row| {
                    let values: Vec<Value> = outs
                        .iter()
                        .map(|o| o.variable(row).and_then(Result::ok).map_or(Value::Null, |v| Built::normalized(v.to_value())))
                        .collect();
                    json!(values)
                })
                .collect()
        })
    }

    fn rows(count: usize, seed: u64) -> Vec<Value> {
        let mut state = seed | 1;
        let mut pick = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        (0..count)
            .map(|_| {
                let accounts: Vec<Value> = (0..pick(8))
                    .map(|_| {
                        let (balance, kind, active) = (pick(5000), ["savings", "checking", "loan"][pick(3) as usize], pick(4) != 0);
                        json!({"balance": balance, "kind": kind, "active": active})
                    })
                    .collect();
                let (age, income, tier, rate) = (16 + pick(64), 1000 + pick(9000), ["gold", "silver", "bronze"][pick(3) as usize], 1 + pick(5));
                json!({"customer": {"age": age, "income": income, "tier": tier, "rate": rate, "accounts": accounts}})
            })
            .collect()
    }

    fn workspace() -> PolicyWorkspace {
        let literal = conformance::literal::Parser::document(Self::POLICY).expect("policy literal");
        let document = conformance::policy::Policy::expand(&literal[0]).expect("policy expand");
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("anchor", serde_json::from_value(document).expect("policy json"));
        ws
    }

    fn time(rows: usize, mut f: impl FnMut() -> f64) -> f64 {
        (0..7).map(|_| f()).fold(f64::MAX, f64::min) / rows as f64
    }

    fn clock(f: impl FnOnce()) -> f64 {
        let start = std::time::Instant::now();
        f();
        start.elapsed().as_nanos() as f64
    }

    fn fresh(rows: &[Variable]) -> Vec<EvaluateRequest> {
        rows.iter()
            .map(|r| EvaluateRequest {
                policy_path: Arc::from("anchor"),
                input: r.depth_clone(usize::MAX),
                goals: Vec::new(),
                trace: false,
            })
            .collect()
    }

    fn floor(columns: &zen_expression::lane::Columns, programs: &mut Vec<Option<zen_expression::lane::LaneProgram>>, runner: &mut zen_expression::lane::LaneRunner, stages: &mut [f64; 8]) {
        use zen_expression::lane::{Column, Columns, Dictionary, LaneProgram, Output, Values};
        let rows = columns.rows;
        let find = |path: &str| columns.find(path).map(|i| columns.columns[i].1);
        let (Some(accounts), Some(rate)) = (find("customer.accounts"), find("customer.rate")) else {
            return;
        };
        let Values::List { offsets, child: Dictionary::Column(record) } = accounts.values else {
            return;
        };
        let parent: Vec<usize> = (0..rows).flat_map(|row| {
            let (a, b) = (offsets[row] as usize, offsets[row + 1] as usize);
            std::iter::repeat_n(row, b - a)
        }).collect();
        let gathered: Vec<rust_decimal::Decimal> = parent.iter().map(|p| rate.number(*p).unwrap_or_default()).collect();
        let field = |name: &str| record.field(name).copied();
        let (Some(balance), Some(kind), Some(active)) = (field("balance"), field("kind"), field("active")) else {
            return;
        };

        let child = Columns::new(parent.len())
            .column("account.balance", balance)
            .column("account.kind", kind)
            .column("account.active", active)
            .column("customer.rate", Column::new(Values::Dec(&gathered)));
        let mut run = |index: usize, columns: &Columns, out: &mut Output, stages: &mut [f64; 8]| {
            let start = std::time::Instant::now();
            let program = programs[index].get_or_insert_with(|| {
                let source = std::env::var(format!("ANCHOR_SOURCE_{index}")).unwrap_or_else(|_| Self::PROGRAMS[index].1.to_string());
                let generic = LaneProgram::standard(&source).expect("program");
                let program = generic.specialize_columns(columns).unwrap_or(generic);
                if std::env::var("ANCHOR_DUMP").is_ok() {
                    eprintln!("== {}\n{:#?}", Self::PROGRAMS[index].0, program.program().steps);
                }
                program
            });
            let repeat = match std::env::var("ANCHOR_REPEAT").ok().and_then(|v| v.parse::<usize>().ok()) {
                Some(only) if only == index => 50,
                _ => 1,
            };
            for _ in 0..repeat {
                runner.evaluate_columns_into(program, columns, out);
            }
            stages[index] += start.elapsed().as_nanos() as f64 / rows as f64 / repeat as f64;
        };
        let mut outs: Vec<Output> = (0..8).map(|_| Output::new()).collect();
        outs[1].prefer_codes();
        outs[4].prefer_codes();
        let [fee, label, total, fees, risk, discount, net, eligible] = &mut outs[..] else {
            return;
        };
        run(0, &child, fee, stages);
        run(1, &child, label, stages);
        let label_offsets: Vec<i32> = label.offsets().iter().map(|o| *o as i32).collect();
        let label_column = match label.codes() {
            Some((keys, offsets, data)) => Column::new(Values::Dict { keys, values: Dictionary::Text { offsets, data } }),
            None => Column::new(Values::Utf8 { offsets: &label_offsets, data: label.data() }),
        };
        let fields = [
            ("balance", balance),
            ("kind", kind),
            ("active", active),
            ("fee", Column::new(Values::Scaled { mant: fee.mantissas(), scale: fee.scales() })),
            ("label", label_column),
        ];
        let composed = Column::new(Values::Struct { fields: &fields, len: parent.len() });
        let list = Column::new(Values::List { offsets, child: Dictionary::Column(&composed) });
        let mut root = Columns::new(rows).column("customer.accounts", list);
        for path in ["customer.age", "customer.income", "customer.tier"] {
            if let Some(column) = find(path) {
                root = root.column(path, column);
            }
        }
        run(2, &root, total, stages);
        run(3, &root, fees, stages);
        let root = root
            .column("customer.total", Column::new(Values::Scaled { mant: total.mantissas(), scale: total.scales() }))
            .column("customer.fees", Column::new(Values::Scaled { mant: fees.mantissas(), scale: fees.scales() }));
        run(4, &root, risk, stages);
        let risk_offsets: Vec<i32> = risk.offsets().iter().map(|o| *o as i32).collect();
        let risk_column = match risk.codes() {
            Some((keys, offsets, data)) => Column::new(Values::Dict { keys, values: Dictionary::Text { offsets, data } }),
            None => Column::new(Values::Utf8 { offsets: &risk_offsets, data: risk.data() }),
        };
        let root = root.column("customer.risk", risk_column);
        run(5, &root, discount, stages);
        let root = root.column("customer.discount", Column::new(Values::Scaled { mant: discount.mantissas(), scale: discount.scales() }));
        run(6, &root, net, stages);
        run(7, &root, eligible, stages);
    }
}

#[test]
fn policy_columnar_shredded_irregular_lists() {
    let mut values = Anchor::rows(96, 11);
    let odd = [
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": []}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": 10, "kind": "loan"}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": 10, "kind": "loan", "active": null}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": "100", "kind": "savings", "active": true}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": 5, "kind": "savings", "active": true, "note": "x"}, {"balance": 7, "kind": "loan", "active": false}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": "oops"}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [1, 2]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"a.b": 1, "balance": 3, "kind": "loan", "active": true}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": 1e30, "kind": "loan", "active": true}, {"balance": -0.0, "kind": "loan", "active": true}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [null]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": 2, "kind": "checking", "active": true, "meta": {"x": 1}, "tags": [1, "a"]}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": 12.5, "kind": "checking", "active": true}, {"kind": "savings", "balance": 0.000000001, "active": true}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{"balance": 4, "kind": 3, "active": "yes"}]}}),
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": 2, "accounts": [{}]}}),
    ];
    for (at, row) in odd.into_iter().enumerate() {
        values.insert(at * 6 + 1, row);
    }
    let ws = Anchor::workspace();
    let built = Built::new(&values).expect("flat columns");
    let columns = built.columns();
    let at = columns.find("customer.accounts").expect("accounts column");
    let mut cells: Vec<Variable> = (0..columns.rows).map(|row| columns.columns[at].1.variable(row)).collect();
    let mut valid = vec![0u64; columns.rows.div_ceil(64)];
    (0..columns.rows).filter(|&row| columns.columns[at].1.valid(row)).for_each(|row| valid[row / 64] |= 1 << (row % 64));
    for row in [3usize, 40] {
        cells[row] = Variable::Null;
        valid[row / 64] |= 1 << (row % 64);
    }
    let mut nulled = zen_expression::lane::Columns::new(columns.rows);
    for (index, (path, column)) in columns.columns.iter().enumerate() {
        nulled = match index == at {
            true => nulled.column(path, zen_expression::lane::Column::with_validity(zen_expression::lane::Values::Any(&cells), &valid, 0)),
            false => nulled.column(path, *column),
        };
    }
    let mut failures = Vec::new();
    for columns in [&columns, &nulled] {
        let output = ws.evaluate_columns(&Arc::from("anchor"), &[], columns).expect("columnar");
        for row in 0..columns.rows {
            let request = EvaluateRequest {
                policy_path: Arc::from("anchor"),
                input: columns.row(row),
                goals: Vec::new(),
                trace: false,
            };
            let want = ws
                .evaluate_with_driver(&request)
                .map(|r| Built::normalized(r.output.to_value()))
                .map_err(|e| format!("{e:?}"));
            let got = match &output.errors[row] {
                Some(error) => Err(format!("{error:?}")),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            if got != want {
                failures.push(format!("row {row} input {}\n  driver   {want:?}\n  columnar {got:?}", columns.row(row).to_value()));
            }
        }
        assert!(output.hosted < columns.rows / 2, "shredded flat input mostly hosted: {}", output.hosted);
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
#[ignore]
fn policy_anchor_throughput() {
    let count: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(1024);
    let values = Anchor::rows(count, 7);
    let rows: Vec<Variable> = values.iter().map(|v| Variable::from(v.clone())).collect();
    let ws = Anchor::workspace();
    let path: Arc<str> = Arc::from("anchor");
    let reference: Vec<Value> = Anchor::fresh(&rows)
        .iter()
        .map(|r| Built::normalized(ws.evaluate_with_driver(r).expect("driver").output.to_value()))
        .collect();
    if let Ok(seconds) = std::env::var("ANCHOR_PROFILE_PLAN") {
        let built = Built::nested(&values).expect("nested columns");
        let fields = built.fields();
        let structs = built.structs(&fields);
        let columns = built.columns_with(&structs);
        let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(4));
        while std::time::Instant::now() < until {
            drop(ws.evaluate_columns(&path, &[], &columns));
        }
        return;
    }
    let mut results: Vec<(&str, f64)> = Vec::new();
    results.push(("driver", Anchor::time(count, || {
        let fresh = Anchor::fresh(&rows);
        Anchor::clock(|| fresh.iter().for_each(|r| drop(ws.evaluate_with_driver(r))))
    })));
    results.push(("compiled batch", Anchor::time(count, || {
        let fresh = Anchor::fresh(&rows);
        Anchor::clock(|| drop(ws.evaluate_batch(&fresh)))
    })));
    let built = Built::new(&values).expect("flat columns");
    let columns = built.columns();
    let flat = ws.evaluate_columns(&path, &[], &columns).expect("columnar flat");
    let flat_wrong = (0..count)
        .filter(|&r| flat.errors[r].is_some() || Built::normalized(flat.row(r).to_value()) != reference[r])
        .count();
    let flat_hosted = flat.hosted;
    drop(flat);
    results.push(("columnar flat", Anchor::time(count, || Anchor::clock(|| drop(ws.evaluate_columns(&path, &[], &columns))))));
    let built = Built::nested(&values).expect("nested columns");
    let fields = built.fields();
    let structs = built.structs(&fields);
    let columns = built.columns_with(&structs);
    let output = ws.evaluate_columns(&path, &[], &columns).expect("columnar");
    let wrong = (0..count)
        .filter(|&r| output.errors[r].is_some() || Built::normalized(output.row(r).to_value()) != reference[r])
        .count();
    if let Ok(seconds) = std::env::var("ANCHOR_PROFILE_COLUMNAR") {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(4));
        while std::time::Instant::now() < until {
            drop(ws.evaluate_columns(&path, &[], &columns));
        }
        return;
    }
    results.push(("columnar struct lists", Anchor::time(count, || Anchor::clock(|| drop(ws.evaluate_columns(&path, &[], &columns))))));
    let mut programs = vec![None; 8];
    let mut runner = zen_expression::lane::LaneRunner::new();
    let mut stages = [0f64; 8];
    Anchor::floor(&columns, &mut programs, &mut runner, &mut stages);
    if let Ok(seconds) = std::env::var("ANCHOR_PROFILE") {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(4));
        let mut each = [0f64; 8];
        while std::time::Instant::now() < until {
            Anchor::floor(&columns, &mut programs, &mut runner, &mut each);
        }
        return;
    }
    let mut best = [f64::MAX; 8];
    let floor = Anchor::time(count, || {
        let mut each = [0f64; 8];
        let t = Anchor::clock(|| Anchor::floor(&columns, &mut programs, &mut runner, &mut each));
        best.iter_mut().zip(each).for_each(|(b, e)| *b = b.min(e));
        t
    });
    results.push(("lane floor (specialized programs)", floor));
    let mut fused_programs = vec![None; 2];
    let mut fused_best = [f64::MAX; 2];
    std::env::set_var("ANCHOR_CHECK", "1");
    let checked = Anchor::fused(&columns, &mut fused_programs, &mut runner, &mut [0f64; 2]).unwrap_or_default();
    std::env::remove_var("ANCHOR_CHECK");
    let fused_wrong = (0..count)
        .filter(|&r| {
            let expected: Vec<Value> = Anchor::KEYS[2..]
                .iter()
                .map(|key| key.split('.').fold(&reference[r], |v, k| &v[k]).clone())
                .collect();
            checked.get(r) != Some(&json!(expected))
        })
        .count();
    if let Ok(seconds) = std::env::var("ANCHOR_PROFILE_FUSED") {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(4));
        while std::time::Instant::now() < until {
            Anchor::fused(&columns, &mut fused_programs, &mut runner, &mut [0f64; 2]);
        }
        return;
    }
    let fused = Anchor::time(count, || {
        let mut each = [0f64; 2];
        let t = Anchor::clock(|| drop(Anchor::fused(&columns, &mut fused_programs, &mut runner, &mut each)));
        fused_best.iter_mut().zip(each).for_each(|(b, e)| *b = b.min(e));
        t
    });
    results.push(("lane floor (fused programs)", fused));
    let driver = results[0].1;
    println!("flat columnar: {flat_wrong} mismatching rows of {count}, {flat_hosted} hosted");
    println!("struct-list columnar: {wrong} mismatching rows of {count}, {} hosted; fused floor: {fused_wrong} mismatching rows", output.hosted);
    for (name, t) in &results {
        println!("{name:36} {t:>10.1} ns/row {:>8.1}x vs driver", driver / t);
    }
    let stages: Vec<String> = Anchor::PROGRAMS.iter().zip(best).map(|((name, _), t)| format!("{name} {t:.0}")).collect();
    println!("lane floor per program (ns/row): {}", stages.join(", "));
    println!("fused floor per program (ns/row): member {:.0}, root {:.0}", fused_best[0], fused_best[1]);
    assert_eq!(fused_wrong, 0, "fused floor differs from the driver");
    assert_eq!(flat_wrong, 0, "flat columnar output differs from the driver");
    assert_eq!(wrong, 0, "struct-list columnar output differs from the driver");
}

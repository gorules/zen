#[path = "support/policy_gen.rs"]
mod policy_gen;
#[allow(dead_code)]
mod conformance;
mod support;
#[allow(dead_code)]
#[path = "support/policies.rs"]
mod policies;

use policies::{Anchor, Batch, Generated, PolicyCase};
use policy_gen::{Gen, Rng};
#[cfg(feature = "driver")]
use serde_json::json;
use serde_json::Value;
use std::sync::Arc;
use support::columns::Built;
use support::golden::{Canon, Live, Outcome, Reference};
use zen_engine::policy::{EvaluateRequest, EvaluationError, EvaluationResult, PolicyWorkspace};
use zen_expression::Variable;

struct Differential {
    reference: Reference,
    compared: usize,
    succeeded: usize,
    failures: Vec<String>,
    kinds: std::collections::BTreeMap<String, usize>,
}

impl Differential {
    fn new(reference: Reference) -> Differential {
        Differential {
            reference,
            compared: 0,
            succeeded: 0,
            failures: Vec::new(),
            kinds: Default::default(),
        }
    }

    fn shape(result: &Result<EvaluationResult, EvaluationError>) -> Result<Value, String> {
        match result {
            Ok(result) => Ok(result.output.clone().into()),
            Err(error) => Err(Reference::debug(error)),
        }
    }

    fn recorded(outcome: &Outcome) -> Result<Value, String> {
        match outcome {
            Outcome::Ok(value) => Ok(Canon::restore(value).into()),
            Outcome::Err { debug, .. } => Err(debug.clone()),
        }
    }

    #[cfg(feature = "driver")]
    fn request(path: &str, input: &Value) -> EvaluateRequest {
        Self::with_goals(path, input, &[])
    }

    fn with_goals(path: &str, input: &Value, goals: &[String]) -> EvaluateRequest {
        EvaluateRequest {
            policy_path: Arc::from(path),
            input: Variable::from(input.clone()),
            goals: goals.iter().map(|g| Arc::from(g.as_str())).collect(),
            trace: false,
        }
    }

    fn driver(&self, suite: &str, ws: &PolicyWorkspace, path: &str, input: &Value, goals: &[String]) -> Result<Value, String> {
        self.reference.policy(
            || Canon::key(suite, goals, &Variable::from(input.clone())),
            || Self::shape(&Live::policy(ws, &Self::with_goals(path, input, goals))),
            Self::recorded,
        )
    }

    fn computed(&self, suite: &str, ws: &PolicyWorkspace, path: &str, inputs: &[Value]) -> Vec<String> {
        inputs
            .iter()
            .find_map(|input| self.driver(suite, ws, path, input, &[]).ok().map(|output| PolicyCase::computed(&output, input)))
            .unwrap_or_default()
    }

    fn run(&mut self, label: &str, ws: &PolicyWorkspace, path: &str, inputs: &[Value]) {
        let computed = self.computed(label, ws, path, inputs);
        let requests: Vec<EvaluateRequest> = inputs
            .iter()
            .enumerate()
            .map(|(i, input)| Self::with_goals(path, input, &PolicyCase::goals(&computed, i)))
            .collect();
        let batch = ws.evaluate_batch(&requests);
        for (i, (input, batched)) in inputs.iter().zip(&batch).enumerate() {
            let goals = PolicyCase::goals(&computed, i);
            let want = self.driver(label, ws, path, input, &goals);
            let single = Self::shape(&ws.evaluate(&Self::with_goals(path, input, &goals)));
            let batched = Self::shape(batched);
            self.compared += 1;
            self.succeeded += usize::from(want.is_ok());
            if let Err(error) = &want {
                let kind: String = error.chars().take_while(|c| c.is_alphanumeric()).collect();
                *self.kinds.entry(kind).or_default() += 1;
            }
            if self.reference.ordered(|| Canon::key(label, &goals, &Variable::from(input.clone()))) {
                continue;
            }
            if single != want {
                self.failures.push(format!("{label} single {input}\n  driver   {want:?}\n  compiled {single:?}"));
            }
            if batched != want {
                self.failures.push(format!("{label} batch {input}\n  driver   {want:?}\n  compiled {batched:?}"));
            }
        }
    }

    fn finish(mut self) {
        self.failures.extend(self.reference.missing());
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
    if Reference::unavailable("policy-fixtures") {
        return;
    }
    let mut differential = Differential::new(Reference::open("policy-fixtures"));
    for case in PolicyCase::load_all() {
        differential.run(&case.name, &case.workspace, &case.path, &case.inputs);
    }
    differential.finish();
}

#[test]
fn compiled_policies_match_the_driver_on_generated_documents() {
    if Reference::unavailable("generated/policy") {
        return;
    }
    let mut differential = Differential::new(Reference::open("generated/policy"));
    for seed in 0..Generated::seeds() {
        let Some(ws) = Generated::workspace(seed) else {
            continue;
        };
        let inputs = Generated::inputs(seed);
        let label = Generated::label(seed);
        if differential.reference.stale(|| Canon::key(&label, &[], &Variable::from(inputs[0].clone())), &Generated::fingerprint(seed)) {
            differential.failures.push(format!("{label}: generated document changed since the goldens were recorded"));
        }
        differential.run(&label, &ws, Generated::PATH, &inputs);
        Columnar::check(&differential.reference, &label, &ws, &inputs, &mut differential.failures);
    }
    differential.finish();
}

struct Columnar;

impl Columnar {
    fn check(reference: &Reference, label: &str, ws: &PolicyWorkspace, inputs: &[Value], failures: &mut Vec<String>) {
        for built in Built::new(inputs).into_iter().chain(Built::lists(inputs)) {
            Self::compare(reference, label, ws, &built.columns(), failures);
        }
        if let Some(built) = Built::nested(inputs) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            Self::compare(reference, label, ws, &built.columns_with(&structs), failures);
        }
    }

    fn compare(reference: &Reference, label: &str, ws: &PolicyWorkspace, columns: &zen_expression::lane::Columns, failures: &mut Vec<String>) {
        let store = zen_engine::ColumnStore::new();
        let Ok(output) = ws.evaluate_columns(&Arc::from(Generated::PATH), &[], columns, &store) else {
            return;
        };
        for row in 0..columns.rows {
            let input = columns.row(row);
            let request = EvaluateRequest {
                policy_path: Arc::from(Generated::PATH),
                input: input.depth_clone(64),
                goals: Vec::new(),
                trace: false,
            };
            let want = reference.policy(
                || Canon::key(label, &[], &input),
                || Live::policy(&ws, &request).map(|r| Built::normalized(r.output.to_value())).map_err(|e| format!("{e:?}")),
                |outcome| Differential::recorded(outcome).map(Built::normalized),
            );
            let got = match &output.errors[row] {
                Some(error) => Err(Reference::debug(error)),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            if got != want && !reference.ordered(|| Canon::key(label, &[], &input)) {
                failures.push(format!("{label} columnar row {row} input {}\n  driver   {want:?}\n  columnar {got:?}", input.to_value()));
            }
        }
    }
}

#[cfg(feature = "driver")]
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
                            let store = zen_engine::ColumnStore::new();
                            drop(ws.evaluate_columns(&Arc::from("generated.json"), &[], &columns, &store));
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
                let store = zen_engine::ColumnStore::new();
                drop(ws.evaluate_columns(&Arc::from("generated.json"), &[], &columns, &store));
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

#[tokio::test]
async fn engine_columns_match_single_evaluations() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{DecisionEngine, EvaluationOptions};

    let loader = Arc::new(MemoryLoader::default());
    let mut rng = Rng(13);
    let document = serde_json::from_value(Gen::document(3)).expect("policy document");
    loader.add("policy", DecisionContent::Policy(PolicyContent(Arc::new(document))));
    let graph: DecisionContent = serde_json::from_str(
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/graphs/loan-approval.json"))
            .or_else(|_| std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data/loan-approval.json")))
            .expect("graph fixture"),
    )
    .expect("graph content");
    loader.add("graph", graph);
    let values: Vec<Value> = (0..40).map(|_| Gen::valid_input(&mut rng)).collect();
    let built = Built::new(&values).expect("flat columns");
    let columns = built.columns();
    let engine = DecisionEngine::default().with_loader(loader);
    for compiled in [false, true] {
        if compiled {
            engine.compile();
        }
        for key in ["policy", "graph", "policy.json"] {
            let store = zen_engine::ColumnStore::new();
            let output = engine.evaluate_columns(key, &columns, &store, EvaluationOptions::default()).await;
            assert_eq!(output.rows, values.len());
            for row in 0..values.len() {
                // The row as the columns hold it, decided alone: the same
                // value exactly (nulls and empty objects included).
                let single = engine
                    .evaluate_with_opts(key, columns.row(row), EvaluationOptions::default())
                    .await
                    .map(|r| r.result.to_value())
                    .map_err(|e| e.to_string());
                let columnar = match &output.errors[row] {
                    Some(error) => Err(error.to_string()),
                    None => Ok(output.row(row).to_value()),
                };
                assert_eq!(columnar.is_ok(), single.is_ok(), "{key} compiled={compiled} row {row}: {columnar:?} vs {single:?}");
                if let (Ok(columnar), Ok(single)) = (columnar, single) {
                    assert_eq!(columnar, single, "{key} compiled={compiled} row {row}");
                }
            }
        }
    }
}

#[tokio::test]
async fn engine_columns_keep_nulls_and_empty_objects() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{DecisionEngine, EvaluationOptions};

    let literal = conformance::literal::Parser::document(
        r#"{models: {a: {x: "number?", y: "number"}}, blocks: [{expression: "a.n", value: "a.x == null ? null : a.x * 2"}, {expression: "a.o", value: "a.y > 1 ? {} : {k: a.y}"}, {expression: "a.z", value: "a.y + 1"}]}"#,
    )
    .expect("policy literal");
    let document = conformance::policy::Policy::expand(&literal[0]).expect("policy expand");
    let loader = Arc::new(MemoryLoader::default());
    loader.add("nulls", DecisionContent::Policy(PolicyContent(Arc::new(serde_json::from_value(document).expect("policy json")))));
    let engine = DecisionEngine::default().with_loader(loader);
    engine.compile();
    let values: Vec<Value> = (0..64)
        .map(|i| match i % 3 {
            0 => serde_json::json!({ "a": { "y": i } }),
            1 => serde_json::json!({ "a": { "x": i, "y": 1 } }),
            _ => serde_json::json!({ "a": { "x": i, "y": i } }),
        })
        .collect();
    let built = Built::new(&values).expect("flat columns");
    let columns = built.columns();
    let store = zen_engine::ColumnStore::new();
    let output = engine.evaluate_columns("nulls", &columns, &store, EvaluationOptions::default()).await;
    for row in 0..values.len() {
        let single = engine
            .evaluate_with_opts("nulls", columns.row(row), EvaluationOptions::default())
            .await
            .expect("decides")
            .result
            .to_value();
        assert!(output.errors[row].is_none(), "row {row}: {:?}", output.errors[row]);
        assert_eq!(output.row(row).to_value(), single, "row {row} (typed {}, laned {})", output.typed, output.laned);
    }
}

#[tokio::test]
async fn engine_graph_columns_keep_nulls_and_empty_objects() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::DecisionContent;
    use zen_engine::{DecisionEngine, EvaluationOptions};

    let graph: DecisionContent = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "type": "inputNode", "name": "request", "position": { "x": 0, "y": 0 } },
            { "id": "ex", "type": "expressionNode", "name": "e", "position": { "x": 200, "y": 0 },
              "content": { "expressions": [
                  { "id": "1", "key": "n", "value": "x == null ? null : x * 2" },
                  { "id": "2", "key": "o", "value": "y > 1 ? {} : {k: y}" },
                  { "id": "3", "key": "z", "value": "y + 1" }
              ] } },
            { "id": "out", "type": "outputNode", "name": "response", "position": { "x": 400, "y": 0 } }
        ],
        "edges": [
            { "id": "a", "sourceId": "in", "targetId": "ex", "type": "edge" },
            { "id": "b", "sourceId": "ex", "targetId": "out", "type": "edge" }
        ]
    }))
    .expect("graph");
    let loader = Arc::new(MemoryLoader::default());
    loader.add("g", graph);
    let engine = DecisionEngine::default().with_loader(loader);
    engine.compile();
    let values: Vec<Value> = (0..64)
        .map(|i| match i % 3 {
            0 => json!({ "y": i }),
            1 => json!({ "x": i, "y": 1 }),
            _ => json!({ "x": i, "y": i }),
        })
        .collect();
    let built = Built::new(&values).expect("flat columns");
    let columns = built.columns();
    let store = zen_engine::ColumnStore::new();
    let output = engine.evaluate_columns("g", &columns, &store, EvaluationOptions::default()).await;
    for row in 0..values.len() {
        let single = engine
            .evaluate_with_opts("g", columns.row(row), EvaluationOptions::default())
            .await
            .expect("decides")
            .result
            .to_value();
        assert!(output.errors[row].is_none(), "row {row}: {:?}", output.errors[row]);
        assert_eq!(output.row(row).to_value(), single, "row {row} (typed {}, laned {})", output.typed, output.laned);
    }
}

impl Anchor {
    #[cfg(feature = "driver")]
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

    #[cfg(feature = "driver")]
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

    #[cfg(feature = "driver")]
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

    fn check(reference: &Reference, suite: &str, ws: &PolicyWorkspace, columns: &zen_expression::lane::Columns, failures: &mut Vec<String>) -> (usize, usize, usize) {
        let store = zen_engine::ColumnStore::new();
        let output = ws.evaluate_columns(&Arc::from(Anchor::PATH), &[], columns, &store).expect("columnar");
        for row in 0..columns.rows {
            let input = columns.row(row);
            let request = EvaluateRequest {
                policy_path: Arc::from(Anchor::PATH),
                input: input.depth_clone(64),
                goals: Vec::new(),
                trace: false,
            };
            let want = reference.policy(
                || Canon::key(suite, &[], &input),
                || Live::policy(&ws, &request).map(|r| Built::normalized(r.output.to_value())).map_err(|e| format!("{e:?}")),
                |outcome| Differential::recorded(outcome).map(Built::normalized),
            );
            let got = match &output.errors[row] {
                Some(error) => Err(Reference::debug(error)),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            if got != want && !reference.ordered(|| Canon::key(suite, &[], &input)) {
                failures.push(format!("row {row} input {}\n  driver   {want:?}\n  columnar {got:?}", input.to_value()));
            }
        }
        (output.typed, output.laned, output.hosted)
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

    #[cfg(feature = "driver")]
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

impl Batch {
    fn checked(&self, reference: &Reference, ws: &PolicyWorkspace, failures: &mut Vec<String>) -> Vec<(usize, usize, usize)> {
        let mut counts = Vec::new();
        let built = Built::new(&self.values).expect("flat columns");
        counts.push(Anchor::check(reference, self.suite, ws, &built.columns(), failures));
        if let Some(built) = Built::nested(&self.values) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            counts.push(Anchor::check(reference, self.suite, ws, &built.columns_with(&structs), failures));
        }
        counts
    }
}

#[cfg(feature = "driver")]
#[test]
fn policy_columnar_shredded_irregular_lists() {
    let mut values = Anchor::rows(96, 11);
    let odd = [
        json!({"customer": {"age": 30, "income": 4000, "tier": "gold", "rate": -0.0, "accounts": [{"balance": 3, "kind": "loan", "active": true}]}}),
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
        let store = zen_engine::ColumnStore::new();
        let output = ws.evaluate_columns(&Arc::from("anchor"), &[], columns, &store).expect("columnar");
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
fn policy_column_order_is_path_independent() {
    let cases = [
        (
            r#"{models: {a: {x: "number"}}, blocks: [{expression: "a.ok", value: "a.x - 1"}, {expression: "a.big", value: "a.x * a.x"}]}"#,
            r#"[{"a": {"x": 5}}, {"a": {"x": 1000000000000000}}]"#,
            ["a.x", "a.big", "a.ok"].as_slice(),
        ),
        (
            r#"{models: {a: {x: "number", y: "number"}}, blocks: [{expression: "a.q", value: "a.x / a.y"}, {table: "dt", inputs: ["a.q"], outputs: ["a.band"], rules: [["> 1", "'big'"], ["", "'small'"]]}]}"#,
            r#"[{"a": {"x": 4, "y": 2}}, {"a": {"x": 4, "y": 0}}]"#,
            ["a.x", "a.y", "a.q", "a.band"].as_slice(),
        ),
        (
            r#"{models: {a: {x: "number", y: "number"}}, blocks: [{expression: "a.q", value: "a.x / a.y"}, {match: "a.band", arms: [["a.q > 1", "'big'"], ["", "'small'"]]}]}"#,
            r#"[{"a": {"x": 4, "y": 2}}, {"a": {"x": 4, "y": 0}}]"#,
            ["a.x", "a.y", "a.q", "a.band"].as_slice(),
        ),
    ];
    for (policy, rows, order) in cases {
        let ws = Anchor::workspace_with(policy);
        let pair: Vec<Value> = serde_json::from_str(rows).expect("rows");
        let values: Vec<Value> = (0..64).map(|row| pair[row % 2].clone()).collect();
        let built = Built::new(&values).expect("flat columns");
        let columns = built.columns();
        for call in 0..8 {
            let store = zen_engine::ColumnStore::new();
            let output = ws.evaluate_columns(&Arc::from("anchor"), &[], &columns, &store).expect("columnar");
            let got: Vec<&str> = output.columns.iter().map(|(path, _)| path.as_ref()).collect();
            assert_eq!(got, order, "call {call}: typed {}, laned {}", output.typed, output.laned);
            assert!((0..output.rows).all(|row| output.errors[row].is_some() == (row % 2 == 1)), "call {call}: errors");
        }
    }
}

#[test]
fn policy_tables_match_coded_nulls_against_null_cells() {
    use zen_expression::lane::{Column, Columns, Dictionary, Values};
    let ws = Anchor::workspace_with(r#"{models: {a: {k: "string?"}}, blocks: [{table: "cls", inputs: ["a.k"], outputs: ["a.cls"], rules: [["null", "'null'"], ["", "'other'"]]}]}"#);
    let keys = [0i32, -1, 0, -1, 1];
    let column = Column::new(Values::Dict {
        keys: &keys,
        values: Dictionary::Text { offsets: &[0, 1, 1], data: "x" },
    });
    let columns = Columns::new(keys.len()).column("a.k", column);
    let want = ["other", "null", "other", "null", "other"];
    for call in 0..4 {
        let store = zen_engine::ColumnStore::new();
        let output = ws.evaluate_columns(&Arc::from("anchor"), &[], &columns, &store).expect("columnar");
        for (row, want) in want.iter().enumerate() {
            assert!(output.errors[row].is_none(), "call {call} row {row}: {:?}", output.errors[row]);
            let cls = output.row(row).dot("a.cls").map(|v| v.to_value());
            assert_eq!(cls, Some(Value::from(*want)), "call {call} row {row}");
        }
    }
}

#[test]
fn policy_anchor_is_fully_typed() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    let batch = Anchor::base();
    let mut failures = Vec::new();
    let counts = batch.checked(&reference, &batch.workspace(), &mut failures);
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    for (typed, laned, hosted) in counts {
        assert_eq!(hosted, 0, "anchor rows hosted");
        if Anchor::typed() {
            assert_eq!((typed, laned), (8, 0), "anchor units typed / laned");
        }
    }
}

#[test]
fn policy_typed_falls_back_per_unit() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    let batch = Anchor::fallback();
    assert_ne!(batch.policy, Anchor::POLICY);
    let mut failures = Vec::new();
    let counts = batch.checked(&reference, &batch.workspace(), &mut failures);
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    for (typed, laned, hosted) in counts {
        assert_eq!(hosted, 0, "rows hosted");
        if Anchor::typed() {
            assert!(typed > 0 && laned > 0, "units typed {typed}, laned {laned}");
            assert_eq!(typed + laned, 9, "units typed {typed}, laned {laned}");
        }
    }
}

#[test]
fn policy_typed_fuses_lane_runs() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    assert_ne!(Anchor::fused_policy(), Anchor::POLICY);
    let ws = Anchor::workspace_with(&Anchor::fused_policy());
    let mut failures = Vec::new();
    let counts: Vec<(usize, usize, usize)> = [5, 6, 7].into_iter().flat_map(|seed| Anchor::fusion(seed).checked(&reference, &ws, &mut failures)).collect();
    for (call, (typed, laned, hosted)) in counts.into_iter().enumerate() {
        assert_eq!(hosted, 0, "rows hosted");
        if Anchor::typed() {
            assert_eq!(typed + laned, 10, "call {call}: units typed {typed}, laned {laned}");
            if call < 2 {
                assert!(typed > 0 && laned >= 2, "call {call}: units typed {typed}, laned {laned}");
            }
        }
    }
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn policy_typed_member_tables_and_math() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    let policy = Anchor::members_policy();
    assert!(policy.contains("account.band") && policy.contains("customer.square"));
    let ws = Anchor::workspace_with(&policy);
    let mut failures = Vec::new();
    let counts: Vec<(usize, usize, usize)> = [5, 9].into_iter().flat_map(|seed| Anchor::members(seed).checked(&reference, &ws, &mut failures)).collect();
    for (call, (typed, laned, hosted)) in counts.into_iter().enumerate() {
        assert_eq!(hosted, 0, "rows hosted");
        if Anchor::typed() {
            assert_eq!(typed + laned, 15, "call {call}: units typed {typed}, laned {laned}");
            if call < 2 {
                assert_eq!((typed, laned), (15, 0), "call {call}: units typed {typed}, laned {laned}");
            }
        }
    }
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn policy_typed_lists_and_closures() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    let batch = Anchor::lists();
    let mut failures = Vec::new();
    for (typed, laned, hosted) in batch.checked(&reference, &batch.workspace(), &mut failures) {
        eprintln!("lists typed {typed} laned {laned} hosted {hosted}");
        if Anchor::typed() {
            assert!(typed > 0, "units typed {typed}, laned {laned}");
        }
    }
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn policy_typed_strings() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    let [first, second] = Anchor::strings();
    let ws = first.workspace();
    let mut failures = Vec::new();
    for (typed, laned, hosted) in first.checked(&reference, &ws, &mut failures) {
        eprintln!("strings typed {typed} laned {laned} hosted {hosted}");
        if Anchor::typed() {
            assert!(typed > laned, "units typed {typed}, laned {laned}");
        }
    }
    second.checked(&reference, &ws, &mut failures);
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn policy_typed_dates() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    let [first, second] = Anchor::dates();
    let ws = first.workspace();
    let mut failures = Vec::new();
    for (typed, laned, hosted) in first.checked(&reference, &ws, &mut failures) {
        eprintln!("dates typed {typed} laned {laned} hosted {hosted}");
        if Anchor::typed() {
            assert!(typed > laned, "units typed {typed}, laned {laned}");
        }
    }
    second.checked(&reference, &ws, &mut failures);
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn policy_typed_extremes() {
    if Reference::unavailable("anchor") {
        return;
    }
    let reference = Reference::open("anchor");
    let [first, second] = Anchor::extremes();
    let ws = Anchor::workspace();
    let mut failures = Vec::new();
    let counts = first.checked(&reference, &ws, &mut failures);
    assert_eq!(counts.len(), 2, "flat and struct-list columns");
    for (typed, laned, hosted) in counts {
        eprintln!("extremes typed {typed} laned {laned} hosted {hosted}");
        assert_eq!(hosted, 0, "hosted rows");
        if Anchor::typed() {
            assert!(typed > 0, "anchor units typed {typed}");
            assert_eq!(typed + laned, 8, "anchor units typed {typed} / laned {laned}");
        }
    }
    second.checked(&reference, &ws, &mut failures);
    failures.extend(reference.missing());
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
}

#[test]
#[ignore]
fn single_row_latency() {
    let count: usize = std::env::var("BENCH_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    let rows: Vec<Variable> = Anchor::rows(count, 7).into_iter().map(Variable::from).collect();
    let ws = Anchor::workspace();
    let traced = |rows: &[Variable]| -> Vec<EvaluateRequest> { Anchor::fresh(rows).into_iter().map(|r| EvaluateRequest { trace: true, ..r }).collect() };
    let mut results: Vec<(&str, f64)> = vec![
        ("evaluate", Anchor::time(count, || {
            let fresh = Anchor::fresh(&rows);
            Anchor::clock(|| fresh.iter().for_each(|r| drop(ws.evaluate(r))))
        })),
        ("evaluate traced", Anchor::time(count, || {
            let fresh = traced(&rows);
            Anchor::clock(|| fresh.iter().for_each(|r| drop(ws.evaluate(r))))
        })),
        ("enhance_trace", Anchor::time(count, || {
            let fresh = Anchor::fresh(&rows);
            Anchor::clock(|| fresh.iter().for_each(|r| drop(ws.enhance_trace(r))))
        })),
    ];
    if cfg!(feature = "driver") {
        results.push(("driver", Anchor::time(count, || {
            let fresh = Anchor::fresh(&rows);
            Anchor::clock(|| fresh.iter().for_each(|r| drop(Live::policy(&ws, r))))
        })));
        results.push(("driver traced", Anchor::time(count, || {
            let fresh = traced(&rows);
            Anchor::clock(|| fresh.iter().for_each(|r| drop(Live::policy(&ws, r))))
        })));
    }
    for (name, t) in results {
        println!("{name:24} {t:>10.0} ns/eval");
    }
}


#[cfg(feature = "driver")]
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
            let store = zen_engine::ColumnStore::new();
            drop(ws.evaluate_columns(&path, &[], &columns, &store));
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
    let store = zen_engine::ColumnStore::new();
    let flat = ws.evaluate_columns(&path, &[], &columns, &store).expect("columnar flat");
    let flat_wrong = (0..count)
        .filter(|&r| flat.errors[r].is_some() || Built::normalized(flat.row(r).to_value()) != reference[r])
        .count();
    let flat_hosted = flat.hosted;
    drop(flat);
    let store = zen_engine::ColumnStore::new();
    results.push(("columnar flat", Anchor::time(count, || Anchor::clock(|| drop(ws.evaluate_columns(&path, &[], &columns, &store))))));
    let built = Built::nested(&values).expect("nested columns");
    let fields = built.fields();
    let structs = built.structs(&fields);
    let columns = built.columns_with(&structs);
    let store = zen_engine::ColumnStore::new();
    let output = ws.evaluate_columns(&path, &[], &columns, &store).expect("columnar");
    let wrong = (0..count)
        .filter(|&r| output.errors[r].is_some() || Built::normalized(output.row(r).to_value()) != reference[r])
        .count();
    if let Ok(seconds) = std::env::var("ANCHOR_PROFILE_COLUMNAR") {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(seconds.parse().unwrap_or(4));
        while std::time::Instant::now() < until {
            let store = zen_engine::ColumnStore::new();
            drop(ws.evaluate_columns(&path, &[], &columns, &store));
        }
        return;
    }
    let store = zen_engine::ColumnStore::new();
    results.push(("columnar struct lists", Anchor::time(count, || Anchor::clock(|| drop(ws.evaluate_columns(&path, &[], &columns, &store))))));
    if std::env::var("ZEN_CENSUS").is_ok() {
        for (key, calls, rows, ns) in zen_engine::Decision::census() {
            println!("CENSUS\tanchor\t{key}\t{calls}\t{rows}\t{ns}");
        }
    }
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

/// Text values and validity for a column of optional strings.
fn texts(values: &[Option<&str>]) -> (Vec<i32>, String, Vec<u64>) {
    let (mut offsets, mut data, mut bits) = (vec![0i32], String::new(), vec![0u64; values.len().div_ceil(64).max(1)]);
    for (row, value) in values.iter().enumerate() {
        if let Some(value) = value {
            data.push_str(value);
            bits[row / 64] |= 1 << (row % 64);
        }
        offsets.push(data.len() as i32);
    }
    (offsets, data, bits)
}

#[tokio::test]
async fn engine_prepared_columns_match_single_evaluations() {
    use rust_decimal::Decimal;
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{ColumnStore, DecisionEngine, EvaluationOptions};
    use zen_expression::lane::{Column, Columns, Dictionary, Values};

    // A transaction referencing its customer, dates on both, a default on
    // the customer: what a screening policy over pools reads.
    let property = |model: &str, name: &str, kind: &str, extra: Value| {
        let mut out = serde_json::json!({"id": format!("{model}.{name}"), "name": name, "type": kind, "array": false, "optional": true});
        if let (Some(out), Some(extra)) = (out.as_object_mut(), extra.as_object()) {
            out.extend(extra.clone());
        }
        out
    };
    let document = serde_json::json!({"imports": [], "blocks": [
        {"id": "model:transaction", "type": "dataModel", "props": {"data": {"name": "transaction", "scope": "entity", "properties": [
            property("transaction", "amount", "number", serde_json::json!({})),
            property("transaction", "booked_at", "date", serde_json::json!({})),
            property("transaction", "customer", "reference", serde_json::json!({"target": "customer"})),
        ]}}},
        {"id": "model:customer", "type": "dataModel", "props": {"data": {"name": "customer", "scope": "entity", "properties": [
            property("customer", "id", "string", serde_json::json!({})),
            property("customer", "risk_rating", "string", serde_json::json!({"default": "unknown"})),
            property("customer", "score", "number", serde_json::json!({})),
            property("customer", "valid_from", "date", serde_json::json!({})),
        ]}}},
        {"id": "b0", "type": "expression", "props": {"data": {"key": "transaction.flag", "value": "(transaction.customer.score ?? 0) > 5 or transaction.customer.risk_rating == 'high'"}}},
        {"id": "b1", "type": "expression", "props": {"data": {"key": "transaction.rating", "value": "transaction.customer.risk_rating"}}},
        {"id": "b2", "type": "expression", "props": {"data": {"key": "transaction.big", "value": "(transaction.amount ?? 0) > 100"}}},
        {"id": "b3", "type": "expression", "props": {"data": {"key": "transaction.since", "value": "transaction.customer.valid_from"}}},
    ]});
    let loader = Arc::new(MemoryLoader::default());
    loader.add("screen.json", DecisionContent::Policy(PolicyContent(Arc::new(serde_json::from_value(document).expect("policy json")))));

    // Rows: found; default filled; key not in pool; no key; a date that
    // does not parse; duplicate ids (the last wins); an empty date; a pool
    // item without its date.
    let keys = texts(&[Some("c1"), Some("c2"), Some("c9"), None, Some("c1"), Some("c5"), Some("c1"), Some("c6")]);
    let booked = texts(&[
        Some("2026-01-02T10:00:00Z"),
        Some("2026-01-03T10:00:00Z"),
        Some("2026-01-03T11:00:00Z"),
        Some("2026-01-04T10:00:00Z"),
        Some("nope"),
        Some("2026-01-05T10:00:00Z"),
        Some(""),
        None,
    ]);
    let amounts: Vec<Decimal> = [50, 150, 10, 500, 20, 101, 99, 7].into_iter().map(Decimal::from).collect();
    // Pool items, per row: [c1] [c2] [c1] [] [c1] [c5, c5] [c1] [c6].
    let pool_rows: [&[usize]; 8] = [&[0], &[1], &[0], &[], &[0], &[2, 3], &[0], &[4]];
    let items: [(&str, Option<&str>, i64, Option<&str>); 5] = [
        ("c1", Some("high"), 2, Some("2020-01-01")),
        ("c2", None, 9, Some("2021-05-05")),
        ("c5", Some("low"), 1, Some("2022-02-02")),
        ("c5", Some("medium"), 8, Some("2023-03-03")),
        ("c6", Some("low"), 3, None),
    ];
    let mut offsets = vec![0i32];
    let mut flat: Vec<usize> = Vec::new();
    for row in pool_rows {
        flat.extend_from_slice(row);
        offsets.push(flat.len() as i32);
    }
    let ids = texts(&flat.iter().map(|&i| Some(items[i].0)).collect::<Vec<_>>());
    let ratings = texts(&flat.iter().map(|&i| items[i].1).collect::<Vec<_>>());
    let froms = texts(&flat.iter().map(|&i| items[i].3).collect::<Vec<_>>());
    let scores: Vec<Decimal> = flat.iter().map(|&i| Decimal::from(items[i].2)).collect();
    fn text(t: &(Vec<i32>, String, Vec<u64>)) -> Values<'_> {
        Values::Text { offsets: &t.0, data: &t.1 }
    }
    let fields = [
        ("id", Column::new(text(&ids))),
        ("risk_rating", Column::with_validity(text(&ratings), &ratings.2, 0)),
        ("score", Column::new(Values::Dec(&scores))),
        ("valid_from", Column::with_validity(text(&froms), &froms.2, 0)),
    ];
    let item = Column::new(Values::Struct { fields: &fields, len: flat.len() });
    let columns = Columns::new(8)
        .column("transaction.amount", Column::new(Values::Dec(&amounts)))
        .column("transaction.booked_at", Column::with_validity(text(&booked), &booked.2, 0))
        .column("transaction.customer", Column::with_validity(text(&keys), &keys.2, 0))
        .column("customer", Column::new(Values::List { offsets: &offsets, child: Dictionary::Column(&item) }));

    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(failures.is_empty(), "{failures:?}");
    let store = ColumnStore::new();
    let output = engine.evaluate_columns("screen.json", &columns, &store, EvaluationOptions::default()).await;
    assert_eq!(output.rows, 8);
    for row in 0..8 {
        let single = engine
            .evaluate_with_opts("screen.json", columns.row(row), EvaluationOptions::default())
            .await
            .map(|r| r.result.to_value())
            .map_err(|e| e.to_string());
        let columnar = match &output.errors[row] {
            Some(error) => Err(error.to_string()),
            None => Ok(output.row(row).to_value()),
        };
        assert_eq!(columnar, single, "row {row}");
    }
    // Only the rows the row path must see (a key not in its pool, a date
    // that does not parse) are decided one by one.
    assert_eq!(output.hosted, 2, "rows decided one by one");
    // And why: a key not in its pool, a date that does not parse.
    let mut why: Vec<(String, usize)> = output.fallbacks.iter().map(|f| (format!("{:?}", f.reason), f.rows)).collect();
    why.sort();
    assert_eq!(why, vec![("Date".to_string(), 1), ("Reference".to_string(), 1)]);
}

/// A member match's arm chosen as columns, then its entity handed to the
/// host before the match commits (a later block it can't run per member):
/// the rows go to the row path, never committed without their arm.
#[tokio::test]
async fn engine_columns_keep_member_arms_when_retired() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{DecisionEngine, EvaluationOptions};

    let literal = conformance::literal::Parser::document(r#"{models: {customer: {qty: "number", days: "number?", members: "member[]"}, member: {balance: "number?", kind: "string(retail|corporate)", employees: "employee[]"}, employee: {score: "number", years: "number", flagged: "boolean?"}},
    globals: {settings: {threshold: "number"}},
    blocks: [
      {match: "member.rank", arms: [["not (member.balance == null)", "'DE'"], ["", "string(threshold)"]]},
      {expression: "summary", value: "{a: customer.qty == (customer.days ?? 0), ok: customer.qty}"},
      {match: "member.bucket", arms: [["(summary.ok) / (abs((customer.days ?? 0)) + 1) != threshold", "count(member.employees, not ((#.flagged ?? false)))"], ["", "0"]]},
      {expression: "member.capped", value: "member.bucket + 1"},
      {expression: "member.adjusted", value: "string(count(member.employees, not ((#.flagged ?? false))))"},
      {match: "member.calc.bonus", arms: [["some(member.employees, #.flagged == null)", "(member.adjusted + '-' + 'x')"], ["member.balance == null", "member.rank"], ["member.kind in ['retail', 'corporate']", "trim((split(member.adjusted, '-')[1] ?? 'none'))"], ["", "member.adjusted"]]},
    ]}"#).expect("policy literal");
    let document = conformance::policy::Policy::expand(&literal[0]).expect("policy expand");
    let loader = Arc::new(MemoryLoader::default());
    loader.add("spec.json", DecisionContent::Policy(PolicyContent(Arc::new(serde_json::from_value(document).expect("policy json")))));
    let engine = DecisionEngine::default().with_loader(loader);
    assert!(engine.compile().is_empty());
    let input = conformance::literal::Parser::document(r#"[{threshold: 0, customer: {qty: -500, priority: true, openedAt: "2024-01-01", days: 0, flagged: null, members: [{balance: null, kind: "retail", employees: [{score: 0.5, years: 0.000001, flagged: false}]}]}}, {threshold: 100, customer: {qty: 0, priority: false, openedAt: "2000-01-01", members: [{balance: -0.5, kind: "corporate", employees: []}, {kind: "retail", employees: [{score: 1, years: 123456789.123, flagged: false}, {score: 100, years: 10, flagged: null}, {score: 100, years: 18, flagged: true}]}, {balance: null, kind: "retail", employees: [{score: 99.99, years: 100}, {score: 18, years: 100, flagged: true}]}, {balance: null, kind: "retail", employees: [{score: 65, years: 10, flagged: null}]}, {balance: 123456789.123, kind: "corporate", employees: [{score: 1, years: 1000000, flagged: true}, {score: 1e3, years: 99999999999999999999, flagged: false}]}]}}, {threshold: -2, customer: {qty: 100, priority: true, openedAt: "1990-06-15", days: 0.5, flagged: false, members: [{balance: -0.01, kind: "corporate", employees: [{score: 65, years: 1000, flagged: false}]}, {balance: -500, kind: "retail", employees: [{score: -0.5, years: 18, flagged: false}, {score: 0.000001, years: -0.01, flagged: false}]}, {balance: -500, kind: "corporate", employees: [{score: 100, years: 2, flagged: true}, {score: -1, years: 1, flagged: false}]}, {balance: 1000000, kind: "retail", employees: [{score: 65, years: 133, flagged: true}]}, {balance: 1.5, kind: "corporate", employees: [{score: -1, years: 0, flagged: false}]}], extraField: "x"}}, {threshold: 1, customer: {qty: 0.5, priority: true, openedAt: "2025-03-20", days: 99.99, flagged: false, members: [{balance: 5, kind: "corporate", employees: [{score: 0.1, years: 250, flagged: true}]}, {balance: 0.5, kind: "retail", employees: [{score: -0.01, years: 18, flagged: false}]}, {balance: 99.99, kind: "corporate", employees: [{score: 0.5, years: 100, flagged: true}, {score: 100, years: 100, flagged: false}, {score: 0.5, years: 1000000, flagged: true}]}, {balance: 100, kind: "corporate", employees: [{score: 2.50, years: 100, flagged: true}]}, {balance: 0.5, kind: "retail", employees: [{score: 7.25, years: -500, flagged: true}, {score: -0, years: 250, flagged: true}, {score: 96, years: 1.5, flagged: false}]}]}}, {threshold: 0, customer: {qty: 0.1, priority: true, openedAt: "1990-06-15", days: 18, flagged: true, members: [{balance: 1, kind: "retail", employees: [{score: 100, years: 100, flagged: false}, {score: 100, years: 32, flagged: true}]}], extraField: {nested: true}}}, {threshold: 10, customer: {qty: -500, priority: true, openedAt: "2024-02-29", days: 100, flagged: true, members: [{balance: 1, kind: "corporate", employees: []}, {balance: 2, kind: "corporate", employees: [{score: 1.5, years: 5, flagged: false}, {score: 100, years: 2.50, flagged: false}, {score: -0.01, years: 100, flagged: false}]}, {balance: 0.5, kind: "retail", employees: [{score: 1000000, years: 1.5, flagged: true}, {score: -0.5, years: 2, flagged: true}]}]}}, {threshold: 0, customer: {qty: 1.5, priority: true, openedAt: "1990-06-15", days: "12", flagged: true, members: [{balance: 1.5, kind: "retail", employees: [{score: 1000, years: -0.5, flagged: true}, {score: -0, years: 100, flagged: false}, {score: 250, years: 123456789.123, flagged: true}]}, {balance: 0.5, kind: "retail", employees: []}, {balance: 116, kind: "retail", employees: []}, {balance: 2.50, kind: "corporate", employees: []}, {balance: -1, kind: "retail", employees: [{score: 1000, years: 65, flagged: true}, {score: 1.5, years: 2.50, flagged: false}, {score: -0.5, years: 65, flagged: false}]}, {balance: 2, kind: "corporate", employees: [{score: 2, years: 99.99, flagged: true}]}, {balance: 10, kind: "retail", employees: [{score: -0.01, years: 111, flagged: false}, {score: 65, years: 2.50, flagged: false}]}, {balance: 2, kind: "retail", employees: [{score: 10, years: 10, flagged: true}, {score: 1.5, years: 0.5, flagged: true}, {score: 451, years: -0, flagged: true}]}, {balance: 10, kind: "retail", employees: []}]}}, {threshold: 10, customer: {qty: -0.01, priority: false, openedAt: "2030-12-31", days: 2, flagged: true, members: [{balance: null, kind: "corporate", employees: [{score: 1000000, years: 7.25, flagged: true}, {score: 2, years: 2, flagged: null}, {score: 0.000001, years: 99999999999999999999, flagged: true}]}, {balance: null, kind: "retail", employees: [{score: 7.25, years: 1.5, flagged: false}, {score: 99.99, years: 0.000001}]}, {kind: "corporate", employees: [{score: 100, years: 1000000, flagged: null}, {score: 18, years: -1, flagged: false}]}]}}, {threshold: 1, customer: {qty: 2, priority: true, openedAt: "2024-01-01", days: null, flagged: false, members: [{balance: -1, kind: "retail", employees: [{score: 2.50, years: 99999999999999999999}, {score: 10, years: 0.5, flagged: false}]}]}}, {threshold: -2, customer: {qty: 2, priority: true, openedAt: "2000-01-01", days: null, flagged: null, members: [{kind: "corporate", employees: [{score: -0, years: 1, flagged: true}, {score: 1000000, years: 7.25, flagged: false}]}]}}, {threshold: 0, customer: {qty: 100, priority: true, openedAt: "2024-01-01", flagged: true, members: [{kind: "retail", employees: [{score: 65, years: 100}, {score: 250, years: 0, flagged: null}]}, {kind: "retail", employees: [{score: 1000, years: 99.99, flagged: true}, {score: 100, years: 2.50}]}]}}, {threshold: 1, customer: {qty: 99.99, priority: false, openedAt: "2024-02-29", days: 0.1, flagged: false, members: [{balance: 1000000, kind: "retail", employees: [{score: 10, years: 100, flagged: true}, {score: 1000000, years: 100, flagged: true}]}, {balance: 2, kind: "corporate", employees: [{score: 18, years: 1e3, flagged: true}, {score: 18, years: 0, flagged: false}]}], extraField: "x"}}, {threshold: 0, customer: {qty: 1.5, priority: false, openedAt: "1990-06-15", days: 0.5, flagged: true, members: [{balance: 0, kind: "corporate", employees: [{score: -500, years: 99999999999999999999}]}, {balance: 1000000, kind: "retail", employees: [{score: 0.000001, years: 100, flagged: false}, {score: -0.01, years: 7.25, flagged: true}, {score: -0, years: 0.000001}]}, {kind: "retail", employees: []}, {balance: 0.5, kind: "corporate", employees: [{score: 0, years: -0}]}, {balance: null, kind: "retail", employees: [{score: 7.25, years: 99999999999999999999, flagged: false}, {score: -0.5, years: 1, flagged: true}]}]}}, {threshold: 100, customer: {qty: 99.99, priority: true, openedAt: "2025-03-20T10:15:30Z", days: 1000000, flagged: true, members: [{balance: 10, kind: "retail", employees: []}, {balance: 1, kind: "retail", employees: []}, {balance: -0.01, kind: "retail", employees: [{score: 100, years: 250, flagged: false}]}]}}, {threshold: -2, customer: {qty: 100, priority: false, openedAt: "2025-03-20", days: 99999999999999999999, flagged: false, members: [{balance: 5, kind: "retail", employees: [{score: 1000, years: 1000000, flagged: false}]}, {balance: 0.1, kind: "retail", employees: [{score: 250, years: 1000, flagged: true}, {score: 1e3, years: 10, flagged: false}, {score: 1, years: -0, flagged: false}]}]}}]"#).expect("input literal");
    let values: Vec<Value> = input[0].json().as_array().cloned().expect("inputs");
    let check = |name: &str, columns: &zen_expression::lane::Columns, output: &zen_engine::ColumnarOutput, singles: &[Result<Value, String>]| {
        for row in 0..columns.rows {
            let columnar = match &output.errors[row] { Some(e) => Err(e.to_string()), None => Ok(output.row(row).to_value()) };
            assert_eq!(&columnar, &singles[row], "{name} row {row}");
        }
    };
    for built in Built::new(&values).into_iter().chain(Built::lists(&values)) {
        let columns = built.columns();
        let mut singles = Vec::new();
        for row in 0..columns.rows {
            singles.push(engine.evaluate_with_opts("spec.json", columns.row(row), EvaluationOptions::default()).await.map(|r| r.result.to_value()).map_err(|e| e.to_string()));
        }
        let store = zen_engine::ColumnStore::new();
        let output = engine.evaluate_columns("spec.json", &columns, &store, EvaluationOptions::default()).await;
        check("flat", &columns, &output, &singles);
    }
    for built in Built::nested(&values) {
        let fields = built.fields();
        let structs = built.structs(&fields);
        let columns = built.columns_with(&structs);
        let mut singles = Vec::new();
        for row in 0..columns.rows {
            singles.push(engine.evaluate_with_opts("spec.json", columns.row(row), EvaluationOptions::default()).await.map(|r| r.result.to_value()).map_err(|e| e.to_string()));
        }
        let store = zen_engine::ColumnStore::new();
        let output = engine.evaluate_columns("spec.json", &columns, &store, EvaluationOptions::default()).await;
        check("nested", &columns, &output, &singles);
    }
}

/// Reasons as columns (asked for with `trace`): each row's matched arm and
/// matched table rules, as its own trace says them.
#[tokio::test]
async fn engine_columns_give_reasons() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{DecisionEngine, EvaluationOptions, Reasons};
    use zen_expression::lane::{Column, Columns, Values};

    let literal = conformance::literal::Parser::document(
        r#"{models: {a: {x: "number"}}, blocks: [
            {match: "a.size", arms: [["a.x > 50", "'big'"], ["a.x > 10", "'mid'"], ["", "'small'"]]},
            {table: "bands", inputs: ["a.x"], outputs: ["a.band"], rules: [["> 40", "'high'"], ["> 5", "'mid'"], ["", "'low'"]]}
        ]}"#,
    )
    .expect("policy literal");
    let document = conformance::policy::Policy::expand(&literal[0]).expect("policy expand");
    let loader = Arc::new(MemoryLoader::default());
    loader.add("why.json", DecisionContent::Policy(PolicyContent(Arc::new(serde_json::from_value(document).expect("policy json")))));
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(failures.is_empty(), "{failures:?}");
    let xs: Vec<rust_decimal::Decimal> = [1, 7, 20, 45, 99].into_iter().map(rust_decimal::Decimal::from).collect();
    let columns = Columns::new(xs.len()).column("a.x", Column::new(Values::Dec(&xs)));
    let store = zen_engine::ColumnStore::new();
    let traced = EvaluationOptions { trace: true, max_depth: 10 };
    let output = engine.evaluate_columns("why.json", &columns, &store, traced).await;
    let (mut arms, mut rules) = (None, None);
    for reasons in &output.reasons {
        match reasons {
            Reasons::Match { arms: a, .. } => arms = Some(a.clone()),
            Reasons::Table { rules: r, .. } => rules = Some(r.clone()),
        }
    }
    let (arms, rules) = (arms.expect("the match's arms"), rules.expect("the table's rules"));
    for row in 0..xs.len() {
        let single = engine.evaluate_with_opts("why.json", columns.row(row), traced).await.expect("decided");
        let trace = serde_json::to_value(single.trace.expect("a trace")).expect("trace json");
        let executions = trace["executions"].as_array().expect("executions");
        let of = |kind: &str| executions.iter().find(|e| e["trace"]["kind"] == kind).map(|e| e["trace"].clone()).expect("traced");
        let arm = of("match")["matchedArm"].as_str().map(str::to_string);
        assert_eq!(arms[row].as_deref().map(str::to_string), arm, "row {row}: arm");
        let matched: Vec<u32> = of("decisionTable")["matchedRows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_u64().map(|r| r as u32))
            .collect();
        assert_eq!(rules[row], matched, "row {row}: rules");
    }
}

/// Impact as columns: two engines' columnar outputs compared by row, and an
/// aggregate recorded from contexts holding only the paths it reads, say
/// what the row-by-row comparison says.
#[cfg(all(feature = "driver", feature = "data"))]
#[tokio::test]
async fn impact_as_columns_matches_rows() {
    use zen_engine::data::aggregate::{AggregateSpec, Aggregator};
    use zen_engine::data::impact::{ColumnDiff, ImpactAnalysis};
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::DecisionContent;
    use zen_engine::{ColumnStore, DecisionEngine, EvaluationOptions};

    let graph = |threshold: i64| -> DecisionContent {
        serde_json::from_value(json!({
            "nodes": [
                { "id": "in", "type": "inputNode", "name": "request", "position": { "x": 0, "y": 0 } },
                { "id": "ex", "type": "expressionNode", "name": "e", "position": { "x": 200, "y": 0 },
                  "content": { "expressions": [
                      { "id": "1", "key": "flag", "value": format!("x > {threshold}") },
                      { "id": "2", "key": "twice", "value": "y == null ? null : y * 2" }
                  ] } },
                { "id": "out", "type": "outputNode", "name": "response", "position": { "x": 400, "y": 0 } }
            ],
            "edges": [
                { "id": "a", "sourceId": "in", "targetId": "ex", "type": "edge" },
                { "id": "b", "sourceId": "ex", "targetId": "out", "type": "edge" }
            ]
        }))
        .expect("graph")
    };
    let engine = |threshold: i64| {
        let loader = Arc::new(MemoryLoader::default());
        loader.add("g", graph(threshold));
        let engine = DecisionEngine::default().with_loader(loader);
        engine.compile();
        Arc::new(engine)
    };
    let (baseline, candidate) = (engine(5), engine(3));
    let values: Vec<Value> = (0..40).map(|i| if i % 4 == 0 { json!({ "x": i % 10 }) } else { json!({ "x": i % 10, "y": i }) }).collect();
    let built = Built::new(&values).expect("flat columns");
    let columns = built.columns();
    let spec: AggregateSpec = serde_json::from_value(json!({
        "metrics": {
            "changed": { "count": "changed" },
            "flips": { "count": "after.flag and not before.flag" },
            "byX": { "by": "string(input.x % 3)", "metrics": { "n": { "count": true }, "sumY": { "sum": "input.y ?? 0" } } }
        }
    }))
    .expect("spec");
    let options = EvaluationOptions::default();

    // By rows.
    let comparison = ImpactAnalysis::new(candidate.clone(), baseline.clone()).compare("g", "g").await.expect("compare");
    let inputs: Vec<Variable> = (0..columns.rows).map(|row| columns.row(row)).collect();
    let batch = comparison.run_batch_carrying(inputs.clone(), options).await;
    let mut by_rows = Aggregator::compile(&spec).expect("aggregator");
    for (row, input) in batch.rows.into_iter().zip(inputs) {
        by_rows.record(row.into_aggregate_context(input));
    }

    // As columns.
    let (store_b, store_a) = (ColumnStore::new(), ColumnStore::new());
    let before = baseline.evaluate_columns("g", &columns, &store_b, options).await;
    let after = candidate.evaluate_columns("g", &columns, &store_a, options).await;
    let diff = ColumnDiff::of(&before, &after);
    assert_eq!(serde_json::to_value(diff.summary()).unwrap(), serde_json::to_value(&batch.summary).unwrap(), "summary");
    let mut as_columns = Aggregator::compile(&spec).expect("aggregator");
    let paths = as_columns.reads().expect("reads by path");
    assert!(paths.iter().all(|p| ["changed", "input.x", "input.y", "before.flag", "after.flag"].contains(&p.as_str())), "{paths:?}");
    for row in 0..columns.rows {
        as_columns.record(diff.context(&paths, &columns, &before, &after, row));
    }
    assert_eq!(as_columns.finalize(), by_rows.finalize());
    assert!(diff.summary().changed > 0, "the thresholds differ for some rows");
}

/// A graph whose request is an entity (imported from a policy) decided as
/// columns: each row prepared as the row path prepares it (dates, defaults),
/// a row that does not fit failing as it fails alone.
#[cfg(feature = "driver")]
#[tokio::test]
async fn engine_entity_requests_as_columns() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{ColumnStore, DecisionEngine, EvaluationOptions};
    use zen_expression::lane::{Column, Columns, Values};

    let prop = |name: &str, ty: &str, extra: Value| {
        let mut out = json!({ "id": name, "name": name, "type": ty, "array": false, "optional": true });
        if let (Some(out), Some(extra)) = (out.as_object_mut(), extra.as_object()) {
            out.extend(extra.clone());
        }
        out
    };
    let models = json!({ "imports": [], "blocks": [
        { "id": "b-txn", "type": "dataModel", "props": { "data": { "name": "transaction", "properties": [
            prop("txn_id", "string", json!({})),
            prop("amount", "number", json!({})),
            prop("booked_at", "date", json!({})),
            prop("level", "string", json!({ "default": "normal" })),
            prop("customer", "reference", json!({ "target": "customer" }))
        ] } } },
        { "id": "b-cust", "type": "dataModel", "props": { "data": { "name": "customer", "properties": [
            prop("id", "string", json!({})),
            prop("risk_rating", "string", json!({})),
            // Dated, with a default: a record sent with the request is
            // converted and filled as the row path converts it.
            prop("since", "date", json!({})),
            prop("tier", "string", json!({ "default": "basic" }))
        ] } } }
    ] });
    let graph = json!({
        "imports": ["models/aml"],
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": { "target": "transaction" } },
            { "id": "ex", "name": "ex", "type": "expressionNode", "content": { "expressions": [
                { "id": "x0", "key": "big", "value": "(amount ?? 0) > 9000" },
                { "id": "x1", "key": "risky", "value": "customer.risk_rating == 'high'" },
                { "id": "x2", "key": "level", "value": "level" },
                { "id": "x3", "key": "when", "value": "booked_at == null ? 'none' : booked_at.add(1, 'd')" },
                { "id": "x4", "key": "whenKind", "value": "type(booked_at)" },
                { "id": "x5", "key": "since", "value": "customer.since" },
                { "id": "x6", "key": "sinceKind", "value": "type(customer.since)" },
                { "id": "x7", "key": "tier", "value": "customer.tier" }
            ] } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "ex", "sourceHandle": null },
            { "id": "e2", "sourceId": "ex", "targetId": "out", "sourceHandle": null }
        ]
    });
    let loader = Arc::new(MemoryLoader::default());
    loader.add("models/aml", DecisionContent::Policy(PolicyContent(Arc::new(serde_json::from_value(models).expect("models")))));
    loader.add("g.json", serde_json::from_value::<DecisionContent>(graph).expect("graph"));
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(failures.is_empty(), "{failures:?}");

    // Rows: fine; a default to fill; a date that does not parse; an empty
    // date; a customer sent as its id; fine again.
    let booked = texts(&[Some("2026-01-02T10:00:00Z"), Some("2026-01-03"), Some("nope"), Some(""), Some("2026-01-05T00:00:00Z"), Some("2026-01-06T00:00:00Z")]);
    let ids = texts(&[Some("t1"), Some("t2"), Some("t3"), Some("t4"), Some("t5"), Some("t6")]);
    let amounts: Vec<rust_decimal::Decimal> = [100, 9500, 50, 10000, 7, 9001].into_iter().map(rust_decimal::Decimal::from).collect();
    let levels = texts(&[Some("vip"), None, Some("normal"), None, None, Some("low")]);
    let customer_ids = texts(&[Some("c1"), Some("c2"), Some("c3"), Some("c4"), None, Some("c6")]);
    let risks = texts(&[Some("high"), Some("low"), Some("high"), Some("low"), None, Some("high")]);
    let bare = texts(&[None, None, None, None, Some("c5"), None]);
    let since = texts(&[Some("2020-01-01"), None, Some("2021-06-30"), Some("2022-02-02"), None, Some("nope")]);
    fn text(t: &(Vec<i32>, String, Vec<u64>)) -> Values<'_> {
        Values::Text { offsets: &t.0, data: &t.1 }
    }
    let records = Columns::new(6)
        .column("txn_id", Column::new(text(&ids)))
        .column("amount", Column::new(Values::Dec(&amounts)))
        .column("booked_at", Column::new(text(&booked)))
        .column("level", Column::with_validity(text(&levels), &levels.2, 0))
        .column("customer.id", Column::with_validity(text(&customer_ids), &customer_ids.2, 0))
        .column("customer.risk_rating", Column::with_validity(text(&risks), &risks.2, 0))
        .column("customer.since", Column::with_validity(text(&since), &since.2, 0));
    let with_id = Columns::new(6)
        .column("txn_id", Column::new(text(&ids)))
        .column("amount", Column::new(Values::Dec(&amounts)))
        .column("booked_at", Column::new(text(&booked)))
        .column("level", Column::with_validity(text(&levels), &levels.2, 0))
        .column("customer", Column::with_validity(text(&bare), &bare.2, 0));
    for columns in [&records, &with_id] {
        let store = ColumnStore::new();
        let output = engine.evaluate_columns("g.json", columns, &store, EvaluationOptions::default()).await;
        for row in 0..columns.rows {
            let single = engine
                .evaluate_with_opts("g.json", columns.row(row), EvaluationOptions::default())
                .await
                .map(|r| r.result.to_value())
                .map_err(|e| e.to_string());
            let columnar = match &output.errors[row] {
                Some(error) => Err(error.to_string()),
                None => Ok(output.row(row).to_value()),
            };
            assert_eq!(columnar.is_ok(), single.is_ok(), "row {row}: {columnar:?} vs {single:?}");
            if let (Ok(columnar), Ok(single)) = (columnar, single) {
                assert_eq!(columnar, single, "row {row}");
            }
        }
        assert_eq!(output.hosted, 0, "decided as columns: {:?}", output.fallbacks);
        let decided = (0..columns.rows).filter(|&row| output.errors[row].is_none()).count();
        assert!(decided >= 3, "most rows decide: {decided}");
        // An entity's dates are dates, as in policies.
        assert_eq!(output.row(0).to_value()["whenKind"], json!("date"), "{:?}", output.row(0).to_value());
    }
    // A record sent with the request: its dates are dates, its defaults
    // filled.
    let store = ColumnStore::new();
    let output = engine.evaluate_columns("g.json", &records, &store, EvaluationOptions::default()).await;
    assert_eq!(output.row(0).to_value()["sinceKind"], json!("date"), "{:?}", output.row(0).to_value());
    assert_eq!(output.row(0).to_value()["tier"], json!("basic"), "{:?}", output.row(0).to_value());
}

/// A graph whose input schema declares dates, decided as columns: each
/// declared date converted as the input node converts a row's, and its
/// date methods read as the row path reads them.
#[cfg(feature = "driver")]
#[tokio::test]
async fn engine_schema_dates_as_columns() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::DecisionContent;
    use zen_engine::{ColumnStore, DecisionEngine, EvaluationOptions};
    use zen_expression::lane::{Column, Columns, Values};

    let graph = json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": { "schema": json!({
                "type": "object",
                "properties": {
                    "amount": { "type": "number" },
                    "at": { "type": "string", "format": "date-time" },
                    "due": { "anyOf": [{ "type": "null" }, { "type": "string", "format": "date" }] }
                }
            }).to_string() } },
            { "id": "ex", "name": "ex", "type": "expressionNode", "content": { "expressions": [
                { "id": "x0", "key": "year", "value": "year(at)" },
                { "id": "x1", "key": "plusDay", "value": "at.add(1, 'd')" },
                { "id": "x2", "key": "same", "value": "at" },
                { "id": "x3", "key": "kind", "value": "type(at)" },
                { "id": "x4", "key": "due", "value": "due" },
                { "id": "x5", "key": "big", "value": "amount > 100" }
            ] } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "ex", "sourceHandle": null },
            { "id": "e2", "sourceId": "ex", "targetId": "out", "sourceHandle": null }
        ]
    });
    let loader = Arc::new(MemoryLoader::default());
    loader.add("d.json", serde_json::from_value::<DecisionContent>(graph).expect("graph"));
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(failures.is_empty(), "{failures:?}");
    let amounts: Vec<rust_decimal::Decimal> = [50, 150, 7, 300].into_iter().map(rust_decimal::Decimal::from).collect();
    let at = texts(&[Some("2024-02-28T12:00:00Z"), Some("2025-03-20T10:15:30Z"), Some("2026-01-04T08:30:00Z"), Some("2026-01-05T00:00:00Z")]);
    let due = texts(&[Some("2026-02-01"), None, Some("not a date"), Some("2026-03-01")]);
    fn text(t: &(Vec<i32>, String, Vec<u64>)) -> Values<'_> {
        Values::Text { offsets: &t.0, data: &t.1 }
    }
    let columns = Columns::new(4)
        .column("amount", Column::new(Values::Dec(&amounts)))
        .column("at", Column::new(text(&at)))
        .column("due", Column::with_validity(text(&due), &due.2, 0));
    let store = ColumnStore::new();
    let output = engine.evaluate_columns("d.json", &columns, &store, EvaluationOptions::default()).await;
    for row in 0..columns.rows {
        let single = engine
            .evaluate_with_opts("d.json", columns.row(row), EvaluationOptions::default())
            .await
            .map(|r| r.result.to_value())
            .map_err(|e| e.to_string());
        let columnar = match &output.errors[row] {
            Some(error) => Err(error.to_string()),
            None => Ok(output.row(row).to_value()),
        };
        assert_eq!(columnar, single, "row {row}");
    }
    assert_eq!(output.hosted, 0, "decided as columns: {:?}", output.fallbacks);
}

/// An import names its document as the loader does, with or without
/// `.json`: the policy compiles either way (and decides on columns).
#[cfg(feature = "driver")]
#[tokio::test]
async fn imports_name_documents_with_or_without_json() {
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{ColumnStore, DecisionEngine, EvaluationOptions};
    use rust_decimal::Decimal;
    use zen_expression::lane::{Column, Columns, Values};

    let models = json!({ "imports": [], "blocks": [
        { "id": "m", "type": "dataModel", "props": { "data": { "name": "card", "properties": [
            { "id": "n", "name": "n", "type": "number", "array": false, "optional": true }
        ] } } }
    ] });
    let policy = |import: &str| {
        json!({ "imports": [import], "blocks": [
            { "id": "busy", "type": "expression", "props": { "data": { "key": "card.busy", "value": "(card.n ?? 0) >= 5" } } }
        ] })
    };
    let loader = Arc::new(MemoryLoader::default());
    let add = |key: &str, document: Value| {
        loader.add(key, DecisionContent::Policy(PolicyContent(Arc::new(serde_json::from_value(document).expect("policy json")))));
    };
    add("models/cards", models);
    add("bare", policy("models/cards"));
    add("suffixed", policy("models/cards.json"));
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(failures.is_empty(), "{failures:?}");

    let n = [Decimal::from(3), Decimal::from(7)];
    let columns = Columns::new(2).column("card.n", Column::new(Values::Dec(&n)));
    for key in ["bare", "suffixed"] {
        let store = ColumnStore::new();
        let output = engine.evaluate_columns(key, &columns, &store, EvaluationOptions::default()).await;
        assert_eq!(output.hosted, 0, "{key}: {:?}", output.fallbacks);
        assert_eq!(output.row(1).to_value()["card"]["busy"], json!(true), "{key}");
    }
}

/// A referenced record's field given as null is kept as the row path keeps
/// it (columns read null as absent; the row is then read whole).
#[cfg(feature = "driver")]
#[tokio::test]
async fn engine_columns_keep_a_referenced_records_nulls() {
    use rust_decimal::Decimal;
    use zen_engine::loader::MemoryLoader;
    use zen_engine::model::{DecisionContent, PolicyContent};
    use zen_engine::{ColumnStore, DecisionEngine, EvaluationOptions};
    use zen_expression::lane::{Column, Columns, Dictionary, Values};

    let property = |name: &str, kind: &str, extra: Value| {
        let mut out = json!({"id": name, "name": name, "type": kind, "array": false, "optional": true});
        if let (Some(out), Some(extra)) = (out.as_object_mut(), extra.as_object()) {
            out.extend(extra.clone());
        }
        out
    };
    let document = json!({"imports": [], "blocks": [
        {"id": "model:transaction", "type": "dataModel", "props": {"data": {"name": "transaction", "scope": "entity", "properties": [
            property("amount", "number", json!({})),
            property("customer", "reference", json!({"target": "customer"})),
        ]}}},
        {"id": "model:customer", "type": "dataModel", "props": {"data": {"name": "customer", "scope": "entity", "properties": [
            property("id", "string", json!({})),
            property("score", "number", json!({})),
        ]}}},
        {"id": "b0", "type": "expression", "props": {"data": {"key": "transaction.big", "value": "(transaction.amount ?? 0) > 1"}}},
    ]});
    let loader = Arc::new(MemoryLoader::default());
    loader.add("p.json", DecisionContent::Policy(PolicyContent(Arc::new(serde_json::from_value(document).expect("policy json")))));
    let engine = DecisionEngine::default().with_loader(loader);
    let failures = engine.compile();
    assert!(failures.is_empty(), "{failures:?}");

    // Two rows, each with its customer: the first one's score given as null.
    let amounts = [Decimal::from(5), Decimal::from(0)];
    let keys = (vec![0i32, 2, 4], "c1c2".to_string());
    let ids = (vec![0i32, 2, 4], "c1c2".to_string());
    let scores = [Variable::Null, Variable::Number(Decimal::from(7))];
    let fields = [
        ("id", Column::new(Values::Text { offsets: &ids.0, data: &ids.1 })),
        ("score", Column::new(Values::Any(&scores))),
    ];
    let item = Column::new(Values::Struct { fields: &fields, len: 2 });
    let offsets = [0i32, 1, 2];
    let columns = Columns::new(2)
        .column("transaction.amount", Column::new(Values::Dec(&amounts)))
        .column("transaction.customer", Column::new(Values::Text { offsets: &keys.0, data: &keys.1 }))
        .column("customer", Column::new(Values::List { offsets: &offsets, child: Dictionary::Column(&item) }));
    let store = ColumnStore::new();
    let output = engine.evaluate_columns("p.json", &columns, &store, EvaluationOptions::default()).await;
    assert_eq!(output.hosted, 0, "decided as columns: {:?}", output.fallbacks);
    for row in 0..2 {
        let single = engine
            .evaluate_with_opts("p.json", columns.row(row), EvaluationOptions::default())
            .await
            .map(|r| r.result.to_value())
            .expect("decides");
        assert_eq!(output.row(row).to_value(), single, "row {row}");
    }
    assert_eq!(output.row(0).to_value()["transaction"]["customer"]["score"], Value::Null);
    assert!(output.row(0).to_value()["transaction"]["customer"].as_object().is_some_and(|c| c.contains_key("score")), "{:?}", output.row(0).to_value());
}

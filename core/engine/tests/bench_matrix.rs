#![allow(dead_code)]

mod conformance;
#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/policies.rs"]
mod policies;
#[path = "support/policy_gen.rs"]
mod policy_gen;
mod support;

use conformance::run::{GraphRun, PolicyRun};
use conformance::{Dialect, Spec};
use fixture::{Fixture, NETWORK_BOUND, TIME_BOUND};
use policies::Anchor;
use policy_gen::{Gen, Rng};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use support::columns::Built;
use zen_engine::policy::{EvaluateRequest, PolicyWorkspace};
use zen_engine::{Decision, DecisionEngine, EvaluationOptions};
use zen_expression::Variable;

struct Matrix {
    out: std::fs::File,
    rows: usize,
    reps: usize,
    runtime: tokio::runtime::Runtime,
}

struct GraphJob<'j> {
    corpus: &'static str,
    case: String,
    walker: Decision,
    compiled: Decision,
    cold: Decision,
    engine: Option<(&'j DecisionEngine, &'static str)>,
    rows: Vec<Value>,
}

struct PolicyJob<'j> {
    corpus: &'static str,
    case: String,
    workspace: &'j PolicyWorkspace,
    path: Arc<str>,
    goals: Vec<Arc<str>>,
    engine: Option<(&'j DecisionEngine, &'static str)>,
    cold: Option<&'j dyn Fn() -> PolicyWorkspace>,
    rows: Vec<Value>,
}

impl Matrix {
    fn new() -> Matrix {
        let path = std::env::var("BENCH_OUT").unwrap_or_else(|_| "/tmp/bench-matrix.jsonl".into());
        Matrix {
            out: std::fs::File::create(path).expect("bench output"),
            rows: std::env::var("BENCH_ROWS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(256),
            reps: std::env::var("BENCH_REPS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime"),
        }
    }

    fn cycled(&self, rows: &[Value]) -> Vec<Value> {
        (0..self.rows)
            .map(|i| rows[i % rows.len()].clone())
            .collect()
    }

    fn record(&mut self, kind: &str, corpus: &str, case: &str, mode: &str, rows: usize, ns: f64) {
        if ns.is_finite() {
            let line = json!({ "kind": kind, "corpus": corpus, "case": case, "mode": mode, "rows": rows, "ns": ns });
            writeln!(self.out, "{line}").expect("bench write");
        }
    }

    fn best(
        &self,
        rows: usize,
        mut prepare: impl FnMut() -> Vec<Variable>,
        mut run: impl FnMut(Vec<Variable>),
    ) -> f64 {
        let mut best = f64::INFINITY;
        for _ in 0..self.reps {
            let inputs = prepare();
            let start = Instant::now();
            run(inputs);
            best = best.min(start.elapsed().as_nanos() as f64 / rows.max(1) as f64);
        }
        best
    }

    fn columns(rows: &[Value], run: impl FnOnce(&zen_expression::lane::Columns)) -> bool {
        let nested = std::env::var("BENCH_NESTED").is_ok();
        if nested {
            if let Some(built) = Built::nested(rows) {
                let fields = built.fields();
                let structs = built.structs(&fields);
                run(&built.columns_with(&structs));
                return true;
            }
        }
        if let Some(built) = Built::new(rows) {
            run(&built.columns());
            return true;
        }
        match Built::nested(rows) {
            Some(built) => {
                let fields = built.fields();
                let structs = built.structs(&fields);
                run(&built.columns_with(&structs));
                true
            }
            None => false,
        }
    }

    fn graph(&mut self, job: GraphJob) {
        let rows = self.cycled(&job.rows);
        let count = rows.len();
        let fresh = || {
            rows.iter()
                .map(|r| Variable::from(r.clone()))
                .collect::<Vec<_>>()
        };
        let rt = &self.runtime;
        rt.block_on(async {
            for decision in [&job.walker, &job.compiled] {
                for input in fresh().into_iter().take(2) {
                    let _ = decision.evaluate(input).await;
                }
            }
            let _ = job
                .compiled
                .evaluate_batch(&fresh(), EvaluationOptions::default())
                .await;
        });
        let old = self.best(count, fresh, |inputs| {
            rt.block_on(async {
                for input in inputs {
                    let _ = job.walker.evaluate(input).await;
                }
            })
        });
        let single = self.best(count, fresh, |inputs| {
            rt.block_on(async {
                for input in inputs {
                    let _ = job.compiled.evaluate(input).await;
                }
            })
        });
        let engine = match job.engine {
            Some((engine, key)) => self.best(count, fresh, |inputs| {
                rt.block_on(async {
                    for input in inputs {
                        let _ = engine.evaluate(key, input).await;
                    }
                })
            }),
            None => f64::NAN,
        };
        let batch = self.best(count, fresh, |inputs| {
            rt.block_on(async {
                drop(
                    job.compiled
                        .evaluate_batch(&inputs, EvaluationOptions::default())
                        .await,
                );
            })
        });
        let mut columnar = f64::INFINITY;
        let reps = self.reps;
        Self::columns(&rows, |columns| {
            rt.block_on(async {
                drop(
                    job.compiled
                        .evaluate_columns(columns, EvaluationOptions::default())
                        .await,
                );
                for _ in 0..reps {
                    let start = Instant::now();
                    drop(
                        job.compiled
                            .evaluate_columns(columns, EvaluationOptions::default())
                            .await,
                    );
                    columnar = columnar.min(start.elapsed().as_nanos() as f64 / count as f64);
                }
            })
        });
        let first = || Variable::from(rows[0].clone());
        let cold_old = self.best(
            1,
            || vec![first()],
            |inputs| {
                let walker = job.cold.clone().interpreted();
                rt.block_on(async {
                    for input in inputs {
                        let _ = walker.evaluate(input).await;
                    }
                })
            },
        );
        let cold_new = self.best(
            1,
            || vec![first()],
            |inputs| {
                let decision = job.cold.clone();
                rt.block_on(async {
                    for input in inputs {
                        let _ = decision.evaluate(input).await;
                    }
                })
            },
        );
        for (mode, ns) in [
            ("old.single", old),
            ("new.single", single),
            ("new.engine", engine),
            ("new.batch", batch),
            ("new.columnar", columnar),
            ("old.cold", cold_old),
            ("new.cold", cold_new),
        ] {
            self.record("graph", job.corpus, &job.case, mode, count, ns);
        }
    }

    fn policy(&mut self, job: PolicyJob) {
        let rows = self.cycled(&job.rows);
        let count = rows.len();
        let requests = || -> Vec<EvaluateRequest> {
            rows.iter()
                .map(|r| EvaluateRequest {
                    policy_path: job.path.clone(),
                    input: Variable::from(r.clone()),
                    goals: job.goals.clone(),
                    trace: false,
                })
                .collect()
        };
        let ws = job.workspace;
        for request in requests().iter().take(2) {
            let _ = ws.evaluate_with_driver(request);
            let _ = ws.evaluate(request);
        }
        drop(ws.evaluate_batch(&requests()));
        let time = |run: &dyn Fn(Vec<EvaluateRequest>)| {
            let mut best = f64::INFINITY;
            for _ in 0..self.reps {
                let batch = requests();
                let start = Instant::now();
                run(batch);
                best = best.min(start.elapsed().as_nanos() as f64 / count as f64);
            }
            best
        };
        let old = time(&|batch| batch.iter().for_each(|r| drop(ws.evaluate_with_driver(r))));
        let single = time(&|batch| batch.iter().for_each(|r| drop(ws.evaluate(r))));
        let batch = time(&|batch| drop(ws.evaluate_batch(&batch)));
        let engine = match job.engine {
            Some((engine, key)) => {
                let rt = &self.runtime;
                let fresh = || {
                    rows.iter()
                        .map(|r| Variable::from(r.clone()))
                        .collect::<Vec<_>>()
                };
                self.best(count, fresh, |inputs| {
                    rt.block_on(async {
                        for input in inputs {
                            let _ = engine.evaluate(key, input).await;
                        }
                    })
                })
            }
            None => f64::NAN,
        };
        let mut columnar = f64::INFINITY;
        let reps = self.reps;
        Self::columns(&rows, |columns| {
            drop(ws.evaluate_columns(&job.path, &job.goals, columns));
            for _ in 0..reps {
                let start = Instant::now();
                drop(ws.evaluate_columns(&job.path, &job.goals, columns));
                columnar = columnar.min(start.elapsed().as_nanos() as f64 / count as f64);
            }
        });
        let (mut cold_old, mut cold_new) = (f64::NAN, f64::NAN);
        if let Some(build) = job.cold {
            let first = requests().into_iter().next();
            if let Some(first) = first {
                let measure = |driver: bool| {
                    let mut best = f64::INFINITY;
                    for _ in 0..self.reps {
                        let request = first.clone();
                        let start = Instant::now();
                        let ws = build();
                        match driver {
                            true => drop(ws.evaluate_with_driver(&request)),
                            false => drop(ws.evaluate(&request)),
                        }
                        best = best.min(start.elapsed().as_nanos() as f64);
                    }
                    best
                };
                cold_old = measure(true);
                cold_new = measure(false);
            }
        }
        for (mode, ns) in [
            ("old.single", old),
            ("new.single", single),
            ("new.engine", engine),
            ("new.batch", batch),
            ("new.columnar", columnar),
            ("old.cold", cold_old),
            ("new.cold", cold_new),
        ] {
            self.record("policy", job.corpus, &job.case, mode, count, ns);
        }
    }
}

#[test]
#[ignore]
fn bench_matrix() {
    support::switches();
    let only = std::env::var("BENCH_ONLY").unwrap_or_default();
    let wants = |corpus: &str| only.is_empty() || only.split(',').any(|o| o == corpus);
    let mut matrix = Matrix::new();
    let started = Instant::now();

    if wants("spec-graph") {
        Spec::prepare();
        let (suites, _) = Spec::suites(Dialect::Graph);
        for suite in &suites {
            if suite.cases.is_empty() {
                continue;
            }
            let Ok(run) = GraphRun::new(suite) else {
                continue;
            };
            let rows: Vec<Value> = suite.cases.iter().map(|c| c.input.to_value()).collect();
            let job = GraphJob {
                corpus: "spec-graph",
                case: suite.id(),
                walker: run.walker.clone(),
                compiled: run.uncompiled.clone(),
                cold: run.uncompiled.clone(),
                engine: Some((&run.engine, GraphRun::MAIN)),
                rows,
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.graph(job)));
        }
        eprintln!(
            "spec-graph: {} suites in {:?}",
            suites.len(),
            started.elapsed()
        );
    }

    if wants("fixtures") {
        std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
        let loader = Arc::new(support::create_fs_loader());
        for fixture in Fixture::load_all() {
            if NETWORK_BOUND.contains(&fixture.name.as_str())
                || TIME_BOUND.contains(&fixture.name.as_str())
            {
                continue;
            }
            let decision =
                Decision::from(Arc::new(fixture.content.clone())).with_loader(loader.clone());
            let job = GraphJob {
                corpus: "fixtures",
                case: fixture.name.clone(),
                walker: decision.interpreted(),
                compiled: decision.clone(),
                cold: decision,
                engine: None,
                rows: fixture.variants(),
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.graph(job)));
        }
        eprintln!("fixtures done at {:?}", started.elapsed());
    }

    if wants("spec-policy") {
        Spec::prepare();
        let (suites, _) = Spec::suites(Dialect::Policy);
        for suite in &suites {
            let Ok(run) = PolicyRun::new(suite) else {
                continue;
            };
            if !run.blocking.is_empty() || suite.cases.is_empty() {
                continue;
            }
            let goals = &suite.cases[0].goals;
            let cases: Vec<_> = suite.cases.iter().filter(|c| &c.goals == goals).collect();
            let rows: Vec<Value> = cases.iter().map(|c| c.input.to_value()).collect();
            let cold = || {
                PolicyRun::new(suite)
                    .map(|run| run.workspace)
                    .unwrap_or_default()
            };
            let job = PolicyJob {
                corpus: "spec-policy",
                case: suite.id(),
                workspace: &run.workspace,
                path: Arc::from(PolicyRun::MAIN),
                goals: goals.iter().map(|g| Arc::from(g.as_str())).collect(),
                engine: goals.is_empty().then_some((&run.engine, PolicyRun::MAIN)),
                cold: Some(&cold),
                rows,
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.policy(job)));
        }
        eprintln!(
            "spec-policy: {} suites done at {:?}",
            suites.len(),
            started.elapsed()
        );
    }

    if wants("generated") {
        for seed in 0..40u64 {
            let Ok(parsed) = serde_json::from_value(Gen::document(seed)) else {
                continue;
            };
            let build = || {
                let mut ws = PolicyWorkspace::new();
                if let Ok(parsed) = serde_json::from_value(Gen::document(seed)) {
                    ws.set_policy("generated.json", parsed);
                }
                ws
            };
            let mut ws = PolicyWorkspace::new();
            ws.set_policy("generated.json", parsed);
            let mut rng = Rng(seed.wrapping_mul(7919) | 1);
            let rows: Vec<Value> = (0..64).map(|_| Gen::valid_input(&mut rng)).collect();
            let job = PolicyJob {
                corpus: "generated-policy",
                case: format!("seed-{seed:02}"),
                workspace: &ws,
                path: Arc::from("generated.json"),
                goals: Vec::new(),
                engine: None,
                cold: Some(&build),
                rows,
            };
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.policy(job)));
        }
        let ws = Anchor::workspace();
        let build = Anchor::workspace;
        let job = PolicyJob {
            corpus: "anchor-policy",
            case: "anchor".into(),
            workspace: &ws,
            path: Arc::from(Anchor::PATH),
            goals: Vec::new(),
            engine: None,
            cold: Some(&build),
            rows: Anchor::rows(matrix.rows, 7),
        };
        matrix.policy(job);
        eprintln!("generated done at {:?}", started.elapsed());
    }
    if support::switches().census {
        for (key, calls, rows, ns) in zen_engine::diagnostics::census() {
            println!("CENSUS\t{key}\t{calls}\t{rows}\t{ns}");
        }
    }
}

#[test]
#[ignore]
fn columnar_reasons() {
    support::switches();
    let path = std::env::var("BENCH_OUT").unwrap_or_else(|_| "/tmp/columnar-reasons.jsonl".into());
    let mut out = std::fs::File::create(path).expect("reasons output");
    let rows: usize = 256;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let cycled = |rows_in: &[Value]| -> Vec<Value> {
        (0..rows)
            .map(|i| rows_in[i % rows_in.len()].clone())
            .collect()
    };
    let graph = |out: &mut std::fs::File,
                 corpus: &str,
                 case: String,
                 decision: &Decision,
                 rows: Vec<Value>| {
        Matrix::columns(&rows, |columns| {
            let output =
                runtime.block_on(decision.evaluate_columns(columns, EvaluationOptions::default()));
            let verdict = match zen_engine::diagnostics::plan_verdict(decision, columns) {
                Some(Ok(())) => "planned".to_string(),
                Some(Err(reason)) => reason,
                None => "invalid".to_string(),
            };
            let errors = output.errors.iter().filter(|e| e.is_some()).count();
            let paths = output.paths();
            let line = json!({ "kind": "graph", "corpus": corpus, "case": case, "verdict": verdict, "typed": paths.typed, "laned": paths.laned, "hosted": paths.hosted, "errors": errors, "rows": columns.rows });
            writeln!(out, "{line}").expect("write");
        });
    };
    Spec::prepare();
    let (suites, _) = Spec::suites(Dialect::Graph);
    for suite in &suites {
        if suite.cases.is_empty() {
            continue;
        }
        let Ok(run) = GraphRun::new(suite) else {
            continue;
        };
        let rows = cycled(
            &suite
                .cases
                .iter()
                .map(|c| c.input.to_value())
                .collect::<Vec<_>>(),
        );
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            graph(&mut out, "spec-graph", suite.id(), &run.compiled, rows)
        }));
    }
    std::env::set_var("__ZEN_MOCK_UTC_TIME", "2025-08-19T16:55:02.078Z");
    let loader = Arc::new(support::create_fs_loader());
    for fixture in Fixture::load_all() {
        if NETWORK_BOUND.contains(&fixture.name.as_str())
            || TIME_BOUND.contains(&fixture.name.as_str())
        {
            continue;
        }
        let decision =
            Decision::from(Arc::new(fixture.content.clone())).with_loader(loader.clone());
        let rows = cycled(&fixture.variants());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            graph(&mut out, "fixtures", fixture.name.clone(), &decision, rows)
        }));
    }
    let policy = |out: &mut std::fs::File,
                  corpus: &str,
                  case: String,
                  ws: &PolicyWorkspace,
                  path: &Arc<str>,
                  goals: &[Arc<str>],
                  rows: Vec<Value>| {
        Matrix::columns(&rows, |columns| {
            let line = match ws.evaluate_columns(path, goals, columns) {
                Ok(output) => {
                    let paths = output.paths();
                    let errors = output.errors.iter().filter(|e| e.is_some()).count();
                    let census: Vec<String> = zen_engine::diagnostics::census()
                        .into_iter()
                        .map(|(key, _, rows, _)| format!("{key} [{rows}]"))
                        .collect();
                    json!({ "kind": "policy", "corpus": corpus, "case": case, "typed": paths.typed, "laned": paths.laned, "hosted": paths.hosted, "errors": errors, "rows": columns.rows, "census": census })
                }
                Err(e) => {
                    json!({ "kind": "policy", "corpus": corpus, "case": case, "failed": format!("{e:?}") })
                }
            };
            writeln!(out, "{line}").expect("write");
        });
    };
    let (suites, _) = Spec::suites(Dialect::Policy);
    for suite in &suites {
        let Ok(run) = PolicyRun::new(suite) else {
            continue;
        };
        if !run.blocking.is_empty() || suite.cases.is_empty() {
            continue;
        }
        let goals = &suite.cases[0].goals;
        let rows = cycled(
            &suite
                .cases
                .iter()
                .filter(|c| &c.goals == goals)
                .map(|c| c.input.to_value())
                .collect::<Vec<_>>(),
        );
        let targets: Vec<Arc<str>> = goals.iter().map(|g| Arc::from(g.as_str())).collect();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            policy(
                &mut out,
                "spec-policy",
                suite.id(),
                &run.workspace,
                &Arc::from(PolicyRun::MAIN),
                &targets,
                rows,
            )
        }));
    }
    for seed in 0..40u64 {
        let Ok(parsed) = serde_json::from_value(Gen::document(seed)) else {
            continue;
        };
        let mut ws = PolicyWorkspace::new();
        ws.set_policy("generated.json", parsed);
        let mut rng = Rng(seed.wrapping_mul(7919) | 1);
        let rows = cycled(
            &(0..64)
                .map(|_| Gen::valid_input(&mut rng))
                .collect::<Vec<_>>(),
        );
        policy(
            &mut out,
            "generated-policy",
            format!("seed-{seed:02}"),
            &ws,
            &Arc::from("generated.json"),
            &[],
            rows,
        );
    }
}

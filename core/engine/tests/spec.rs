mod conformance;
mod support;

use conformance::{Case, Dialect, Note, Spec, Suite};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use support::columns::Built;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::nodes::custom::{CustomNodeAdapter, CustomNodeRequest};
use zen_engine::nodes::{NodeError, NodeResponse, NodeResult};
use zen_engine::policy::{EvaluateRequest, EvaluationResult, PolicyWorkspace, Severity};
use zen_engine::{Decision, DecisionEngine, DecisionGraphResponse, EvaluationError, EvaluationOptions};
use zen_expression::Variable;

#[derive(Default)]
struct Tally {
    total: usize,
    failed: usize,
    bugs: usize,
    questions: usize,
}

#[derive(Default)]
struct Report {
    files: BTreeMap<String, Tally>,
    failures: Vec<String>,
    bugs: Vec<String>,
    cases: usize,
}

impl Report {
    fn record(&mut self, suite: &Suite, case: &Case, got: &Result<Variable, String>) {
        let tally = self.files.entry(suite.file.clone()).or_default();
        tally.total += 1;
        self.cases += 1;
        tally.questions += matches!(case.note, Note::Question(_)) as usize;
        match (&case.note, suite.verdict(case, got)) {
            (Note::Bug(note), Some(failure)) => {
                tally.bugs += 1;
                self.bugs.push(format!("{failure} | bug: {note}"));
            }
            (Note::Bug(note), None) => {
                tally.failed += 1;
                self.failures.push(format!("{} | passes now, drop the bug note ({note})", suite.location(case)));
            }
            (_, Some(failure)) => {
                tally.failed += 1;
                self.failures.push(failure);
            }
            (_, None) => {}
        }
    }

    fn finish(self, label: &str, problems: Vec<String>) {
        for (file, t) in &self.files {
            eprintln!("{file}: {} cases, {} failed, {} known bugs, {} questions", t.total, t.failed, t.bugs, t.questions);
        }
        eprintln!(
            "{label}: {} cases, {} failed, {} known bugs, {} malformed",
            self.cases,
            self.failures.len(),
            self.bugs.len(),
            problems.len()
        );
        if std::env::var("SPEC_BUGS").is_ok() {
            eprintln!("known bugs:\n{}", self.bugs.join("\n"));
        }
        Differential::assert(problems, self.failures);
    }
}

struct Differential;

impl Differential {
    fn limit() -> usize {
        std::env::var("SPEC_SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(60)
    }

    fn assert(problems: Vec<String>, failures: Vec<String>) {
        assert!(
            problems.is_empty() && failures.is_empty(),
            "malformed:\n{}\nfailures ({}):\n{}",
            problems.join("\n"),
            failures.len(),
            failures.iter().take(Self::limit()).cloned().collect::<Vec<_>>().join("\n")
        );
    }

    fn compare(suite: &Suite, case: &Case, mode: &str, want: &Result<Variable, String>, got: &Result<Variable, String>, out: &mut Vec<String>) {
        let same = match (want, got) {
            (Ok(a), Ok(b)) => Spec::same(a, b),
            (Err(a), Err(b)) => a == b,
            _ => false,
        };
        if case.engines.is_some() {
            return;
        }
        if !same {
            out.push(format!(
                "{} {mode} | input {}\n  reference {}\n  {mode:9} {}",
                suite.location(case),
                case.input_text,
                Self::show(want),
                Self::show(got)
            ));
        }
    }

    fn show(r: &Result<Variable, String>) -> String {
        match r {
            Ok(v) => Spec::render(v),
            Err(e) => format!("!error ({e})"),
        }
    }
}

#[derive(Debug)]
struct SpecNodes;

impl SpecNodes {
    fn respond(request: &CustomNodeRequest) -> Result<Variable, String> {
        let config = &request.node.config;
        match request.node.kind.as_ref() {
            "echo" => Ok(request.input.clone()),
            "config" => Ok(Variable::from(config.as_ref().clone())),
            "render" => {
                let out = Variable::empty_object();
                for key in config.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default() {
                    let value = request.get_field(&key).map_err(|e| e.to_string())?.unwrap_or(Variable::Null);
                    out.dot_insert(&key, value);
                }
                Ok(out)
            }
            "fail" => Err(config.get("message").and_then(|m| m.as_str()).unwrap_or("custom node failed").to_string()),
            other => Err(format!("unknown spec custom node kind {other:?}")),
        }
    }
}

impl CustomNodeAdapter for SpecNodes {
    fn handle(&self, request: CustomNodeRequest) -> std::pin::Pin<Box<dyn std::future::Future<Output = NodeResult> + '_>> {
        Box::pin(async move {
            Self::respond(&request)
                .map(|output| NodeResponse { output, trace_data: None })
                .map_err(|message| NodeError { trace: None, node_id: request.node.id.clone(), source: message.into() })
        })
    }
}

struct GraphRun {
    walker: Decision,
    compiled: Decision,
    engine: DecisionEngine,
}

impl GraphRun {
    fn new(suite: &Suite) -> Result<GraphRun, String> {
        let content = |value: &Value| -> Result<DecisionContent, String> {
            serde_json::from_value::<DecisionContent>(value.clone()).map_err(|e| format!("{}: invalid content: {e}", suite.name))
        };
        let (plain, built) = (Arc::new(MemoryLoader::default()), Arc::new(MemoryLoader::default()));
        for (key, doc) in &suite.documents {
            plain.add(key.as_str(), content(doc)?);
            built.add(key.as_str(), Self::compiled(content(doc)?));
        }
        let DecisionContent::Graph(graph) = content(&suite.content)? else {
            return Err(format!("{}: graph suite content is not a graph", suite.name));
        };
        let shared = Arc::new(MemoryLoader::default());
        for (key, doc) in &suite.documents {
            shared.add(key.as_str(), content(doc)?);
        }
        shared.add(Self::MAIN, content(&suite.content)?);
        let engine = DecisionEngine::default().with_loader(shared).with_adapter(Arc::new(SpecNodes));
        engine.compile();
        let walker = Decision::from(graph.clone()).with_loader(plain).with_adapter(Arc::new(SpecNodes));
        let mut compiled = Decision::from(graph).with_loader(built).with_adapter(Arc::new(SpecNodes));
        compiled.compile();
        Ok(GraphRun { walker, compiled, engine })
    }

    const MAIN: &'static str = "spec-main";

    fn compiled(content: DecisionContent) -> DecisionContent {
        match content {
            DecisionContent::Graph(mut graph) => {
                Arc::make_mut(&mut graph).compile();
                DecisionContent::Graph(graph)
            }
            other => other,
        }
    }

    fn outcome(result: Result<DecisionGraphResponse, Box<EvaluationError>>) -> Result<Variable, String> {
        result.map(|r| r.result).map_err(|e| Self::error(&e))
    }

    fn error(error: &EvaluationError) -> String {
        serde_json::to_value(error).map(|v| v.to_string()).unwrap_or_else(|_| error.to_string())
    }

    async fn walker(&self, case: &Case) -> Result<Variable, String> {
        Self::outcome(self.walker.evaluate(case.input.depth_clone(64)).await)
    }
}

struct PolicyRun {
    workspace: PolicyWorkspace,
    blocking: Vec<String>,
    gate: bool,
    engine: DecisionEngine,
}

impl PolicyRun {
    const MAIN: &'static str = "main";

    fn new(suite: &Suite) -> Result<PolicyRun, String> {
        let mut workspace = PolicyWorkspace::new();
        let loader = Arc::new(MemoryLoader::default());
        let policy = |value: &Value| serde_json::from_value(value.clone()).map_err(|e| format!("{}: invalid policy: {e}", suite.name));
        let document = |value: &Value| serde_json::from_value::<DecisionContent>(value.clone()).map_err(|e| format!("{}: invalid document: {e}", suite.name));
        workspace.set_policy(Self::MAIN, policy(&suite.content)?);
        loader.add(Self::MAIN, document(&suite.content)?);
        for (key, doc) in &suite.documents {
            match doc.get("nodes") {
                Some(_) => workspace.set_document(key.as_str(), document(doc)?),
                None => workspace.set_policy(key.as_str(), policy(doc)?),
            }
            loader.add(key.as_str(), document(doc)?);
        }
        let mut blocking: Vec<String> = workspace
            .evaluation_diagnostics(Self::MAIN)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| serde_json::to_value(d.code).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default())
            .collect();
        blocking.sort();
        blocking.dedup();
        let engine = DecisionEngine::default().with_loader(loader);
        let gate = suite.diagnostics.is_none();
        Ok(PolicyRun { workspace, blocking, gate, engine })
    }

    fn gated(&self, result: &Result<EvaluationResult, zen_engine::policy::EvaluationError>) -> Result<Variable, String> {
        match self.blocking.is_empty() || !self.gate {
            true => Self::outcome(result),
            false => Err(format!("CompilationErrors {:?}", self.blocking)),
        }
    }

    fn driver(&self, case: &Case) -> Result<Variable, String> {
        self.gated(&self.workspace.evaluate_with_driver(&Self::request(case)))
    }

    fn compiled(&self, case: &Case) -> Result<Variable, String> {
        self.gated(&self.workspace.evaluate(&Self::request(case)))
    }

    fn engine(&self, case: &Case) -> Result<Variable, String> {
        let result = Guard::block(self.engine.evaluate(Self::MAIN, case.input.depth_clone(64)));
        match (&result, self.blocking.is_empty()) {
            (Err(_), false) => Err(format!("CompilationErrors {:?}", self.blocking)),
            (Err(error), true) => match &**error {
                EvaluationError::Policy(error) => Err(format!("{error:?}")),
                other => Err(GraphRun::error(other)),
            },
            (Ok(response), _) => Ok(response.result.clone()),
        }
    }

    fn request(case: &Case) -> EvaluateRequest {
        EvaluateRequest {
            policy_path: Arc::from(Self::MAIN),
            input: case.input.depth_clone(64),
            goals: case.goals.iter().map(|g| Arc::from(g.as_str())).collect(),
            trace: false,
        }
    }

    fn outcome(result: &Result<EvaluationResult, zen_engine::policy::EvaluationError>) -> Result<Variable, String> {
        result.as_ref().map(|r| r.output.clone()).map_err(|e| format!("{e:?}"))
    }
}

struct Guard;

impl Guard {
    fn run<T>(label: String, failures: &mut Vec<String>, work: impl FnOnce() -> T) -> Option<T> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
            Ok(value) => Some(value),
            Err(payload) => {
                let message = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                failures.push(format!("{label} | PANIC: {message}"));
                None
            }
        }
    }

    fn block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}

#[derive(Default)]
struct Engines {
    failures: Vec<String>,
    census: BTreeMap<String, usize>,
    compared: usize,
    columnar: usize,
    hosted: usize,
    nested: usize,
}

impl Engines {
    async fn graph(&mut self, suite: &Suite) {
        let Ok(run) = GraphRun::new(suite) else {
            return;
        };
        let verdict = match run.compiled.compiled_verdict() {
            Some(Ok(())) => "compiled".to_string(),
            Some(Err(reason)) => format!("walker: {reason}"),
            None => "not compiled".to_string(),
        };
        *self.census.entry(verdict).or_default() += 1;
        let mut wants = Vec::new();
        for case in &suite.cases {
            wants.push(run.walker(case).await);
        }
        let contexts: Vec<Variable> = suite.cases.iter().map(|c| c.input.depth_clone(64)).collect();
        let cycled: Vec<Variable> = (0..contexts.len() * 3).map(|i| contexts[i % contexts.len()].depth_clone(64)).collect();
        let batch = run.compiled.evaluate_batch(&cycled, EvaluationOptions::default()).await;
        for (i, case) in suite.cases.iter().enumerate() {
            let single = GraphRun::outcome(run.compiled.evaluate(case.input.depth_clone(64)).await);
            Differential::compare(suite, case, "single", &wants[i], &single, &mut self.failures);
            let engine = GraphRun::outcome(run.engine.evaluate(GraphRun::MAIN, case.input.depth_clone(64)).await);
            Differential::compare(suite, case, "engine", &wants[i], &engine, &mut self.failures);
            for result in batch.iter().skip(i).step_by(suite.cases.len().max(1)) {
                let got = result.as_ref().map(|r| r.result.clone()).map_err(|e| GraphRun::error(e));
                Differential::compare(suite, case, "batch", &wants[i], &got, &mut self.failures);
            }
            self.compared += 1;
        }
        let rows: Vec<Value> = suite.cases.iter().map(|c| c.input.to_value()).collect();
        let cycled_rows: Vec<Value> = (0..rows.len() * 3).map(|i| rows[i % rows.len()].clone()).collect();
        for built in Built::new(&rows).into_iter().chain(Built::lists(&rows)).chain(Built::new(&cycled_rows)).chain(Built::nested(&rows)).chain(Built::nested(&cycled_rows)) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            let output = run.compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
            for row in 0..built.rows {
                let input = columns.row(row);
                let want = match run.walker.evaluate(input.depth_clone(64)).await {
                    Ok(r) => Ok(Built::normalized(r.result.to_value())),
                    Err(e) => Err(GraphRun::error(&e)),
                };
                let got = match &output.errors[row] {
                    Some(e) => Err(GraphRun::error(e)),
                    None => Ok(Built::normalized(output.row(row).to_value())),
                };
                self.columnar += 1;
                let case = suite
                    .cases
                    .iter()
                    .find(|c| Built::normalized(c.input.to_value()) == Built::normalized(input.to_value()))
                    .unwrap_or(&suite.cases[0]);
                if got != want && case.engines.is_none() {
                    self.failures.push(format!(
                        "{} columnar | input {}\n  reference {want:?}\n  columnar  {got:?}",
                        suite.location(case),
                        input.to_value()
                    ));
                }
            }
        }
    }

    fn policy(&mut self, suite: &Suite) {
        let Ok(run) = PolicyRun::new(suite) else {
            return;
        };
        let requests: Vec<EvaluateRequest> = (0..suite.cases.len() * 3).map(|i| PolicyRun::request(&suite.cases[i % suite.cases.len()])).collect();
        let batch = run.workspace.evaluate_batch(&requests);
        for (i, case) in suite.cases.iter().enumerate() {
            let want = run.driver(case);
            let single = run.compiled(case);
            Differential::compare(suite, case, "single", &want, &single, &mut self.failures);
            for result in batch.iter().skip(i).step_by(suite.cases.len().max(1)) {
                let got = run.gated(result);
                Differential::compare(suite, case, "batch", &want, &got, &mut self.failures);
            }
            if case.goals.is_empty() && run.gate {
                let engine = run.engine(case);
                Differential::compare(suite, case, "engine", &want, &engine, &mut self.failures);
            }
            self.compared += 1;
        }
        if !run.blocking.is_empty() && run.gate {
            return;
        }
        let mut goal_sets: Vec<&Vec<String>> = suite.cases.iter().map(|c| &c.goals).collect();
        goal_sets.dedup();
        for goals in goal_sets {
            let cases: Vec<&Case> = suite.cases.iter().filter(|c| &c.goals == goals).collect();
            let rows: Vec<Value> = cases.iter().map(|c| c.input.to_value()).collect();
            let cycled: Vec<Value> = (0..rows.len() * 3).map(|i| rows[i % rows.len()].clone()).collect();
            let targets: Vec<Arc<str>> = goals.iter().map(|g| Arc::from(g.as_str())).collect();
            for built in Built::new(&rows).into_iter().chain(Built::lists(&rows)).chain(Built::new(&cycled)) {
                self.policy_columns(suite, &run, &cases, &targets, &built.columns());
            }
            for built in Built::nested(&rows).into_iter().chain(Built::nested(&cycled)) {
                let fields = built.fields();
                let structs = built.structs(&fields);
                self.nested += built.rows;
                self.policy_columns(suite, &run, &cases, &targets, &built.columns_with(&structs));
            }
        }
    }

    fn policy_columns(&mut self, suite: &Suite, run: &PolicyRun, cases: &[&Case], targets: &[Arc<str>], columns: &zen_expression::lane::Columns) {
        let Ok(output) = run.workspace.evaluate_columns(&Arc::from(PolicyRun::MAIN), targets, columns) else {
            return;
        };
        self.hosted += output.hosted;
        for row in 0..columns.rows {
            let input = columns.row(row);
            let request = EvaluateRequest {
                policy_path: Arc::from(PolicyRun::MAIN),
                input: input.depth_clone(64),
                goals: targets.to_vec(),
                trace: false,
            };
            let want = run.workspace.evaluate_with_driver(&request).map(|r| Built::normalized(r.output.to_value())).map_err(|e| format!("{e:?}"));
            let got = match &output.errors[row] {
                Some(error) => Err(format!("{error:?}")),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            self.columnar += 1;
            if got != want {
                let case = cases
                    .iter()
                    .find(|c| Built::normalized(c.input.to_value()) == Built::normalized(input.to_value()))
                    .copied()
                    .unwrap_or(cases[0]);
                self.failures.push(format!(
                    "{} columnar | input {}\n  reference {want:?}\n  columnar  {got:?}",
                    suite.location(case),
                    input.to_value()
                ));
            }
        }
    }
}

#[test]
fn graph_spec_matches_walker() {
    Spec::prepare();
    let (suites, mut problems) = Spec::suites(Dialect::Graph);
    let mut report = Report::default();
    for suite in &suites {
        let run = match GraphRun::new(suite) {
            Ok(run) => run,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let outcomes = Guard::run(label, &mut report.failures, || {
            Guard::block(async {
                let mut out = Vec::new();
                for case in &suite.cases {
                    out.push(run.walker(case).await);
                }
                out
            })
        });
        for (case, got) in suite.cases.iter().zip(outcomes.unwrap_or_default()) {
            report.record(suite, case, &got);
        }
    }
    report.finish("graph spec", problems);
}

#[test]
fn graph_engines_match_walker() {
    Spec::prepare();
    let (suites, problems) = Spec::suites(Dialect::Graph);
    let mut engines = Engines::default();
    for suite in &suites {
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let mut failures = Vec::new();
        Guard::run(label, &mut failures, || Guard::block(engines.graph(suite)));
        engines.failures.extend(failures);
    }
    for (verdict, count) in &engines.census {
        eprintln!("{verdict}: {count} suites");
    }
    eprintln!(
        "graph engines: {} cases, {} columnar rows, {} mismatches",
        engines.compared,
        engines.columnar,
        engines.failures.len()
    );
    Differential::assert(problems, engines.failures);
}

#[test]
fn policy_spec_matches_driver() {
    Spec::prepare();
    let (suites, mut problems) = Spec::suites(Dialect::Policy);
    let mut report = Report::default();
    for suite in &suites {
        let run = match PolicyRun::new(suite) {
            Ok(run) => run,
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let outcomes = Guard::run(label, &mut report.failures, || {
            suite
                .cases
                .iter()
                .map(|case| run.driver(case))
                .collect::<Vec<_>>()
        });
        for (case, got) in suite.cases.iter().zip(outcomes.unwrap_or_default()) {
            report.record(suite, case, &got);
        }
        if let Some(expected) = &suite.diagnostics {
            if *expected != run.blocking {
                report.failures.push(format!(
                    "{}:{} [{}] | diagnostics expected {expected:?} got {:?}",
                    suite.file, suite.line, suite.name, run.blocking
                ));
            }
        }
    }
    report.finish("policy spec", problems);
}

#[test]
fn policy_engines_match_driver() {
    Spec::prepare();
    let (suites, problems) = Spec::suites(Dialect::Policy);
    let mut engines = Engines::default();
    for suite in &suites {
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let mut failures = Vec::new();
        Guard::run(label, &mut failures, || engines.policy(suite));
        engines.failures.extend(failures);
    }
    eprintln!("policy engines: {} cases, {} columnar rows ({} with struct lists, {} hosted per row), {} mismatches", engines.compared, engines.columnar, engines.nested, engines.hosted, engines.failures.len());
    Differential::assert(problems, engines.failures);
}

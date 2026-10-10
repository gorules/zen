mod conformance;
mod support;

use conformance::run::{GraphRun, Guard, PolicyRun};
use conformance::{Case, Dialect, Note, Spec, Suite};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use support::columns::Built;
use support::golden::{Canon, Live, Outcome, Reference};
use zen_engine::policy::EvaluateRequest;
use zen_engine::EvaluationOptions;
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

struct Engines {
    reference: Reference,
    failures: Vec<String>,
    census: BTreeMap<String, usize>,
    compared: usize,
    columnar: usize,
    hosted: usize,
    nested: usize,
    typed: usize,
    laned: usize,
}

impl Engines {
    fn new(reference: Reference) -> Engines {
        Engines {
            reference,
            failures: Vec::new(),
            census: BTreeMap::new(),
            compared: 0,
            columnar: 0,
            hosted: 0,
            nested: 0,
            typed: 0,
            laned: 0,
        }
    }

    async fn walker(reference: &Reference, run: &GraphRun, suite: &Suite, case: &Case) -> Result<Variable, String> {
        reference
            .graph(|| Canon::key(&suite.id(), &[], &case.input), || run.walker(case), GraphRun::recorded)
            .await
    }

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
            wants.push(Self::walker(&self.reference, &run, suite, case).await);
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
        let _ = zen_engine::Decision::census();
        let rows: Vec<Value> = suite.cases.iter().map(|c| c.input.to_value()).collect();
        let cycled_rows: Vec<Value> = (0..rows.len() * 3).map(|i| rows[i % rows.len()].clone()).collect();
        for built in Built::new(&rows).into_iter().chain(Built::lists(&rows)).chain(Built::new(&cycled_rows)).chain(Built::nested(&rows)).chain(Built::nested(&cycled_rows)) {
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            let output = run.compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
            self.typed += output.typed;
            self.laned += output.laned;
            for row in 0..built.rows {
                let input = columns.row(row);
                let want = self
                    .reference
                    .graph(
                        || Canon::key(&suite.id(), &[], &input),
                        || async {
                            match run.walker.evaluate(input.depth_clone(64)).await {
                                Ok(r) => Ok(Built::normalized(r.result.to_value())),
                                Err(e) => Err(GraphRun::error(&e)),
                            }
                        },
                        |outcome| GraphRun::recorded(outcome).map(|v| Built::normalized(v.to_value())),
                    )
                    .await;
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
                if got != want && case.engines.is_none() && !self.reference.ordered(|| Canon::key(&suite.id(), &[], &input)) {
                    self.failures.push(format!(
                        "{} columnar | input {}\n  reference {want:?}\n  columnar  {got:?}",
                        suite.location(case),
                        input.to_value()
                    ));
                }
            }
        }
        if std::env::var("ZEN_CENSUS").is_ok() {
            for (key, calls, rows, ns) in zen_engine::Decision::census() {
                println!("CENSUS\tspec:{}\t{key}\t{calls}\t{rows}\t{ns}", suite.id());
            }
        }
    }

    fn driver(reference: &Reference, run: &PolicyRun, suite: &Suite, case: &Case) -> Result<Variable, String> {
        reference.policy(|| Canon::key(&suite.id(), &case.goals, &case.input), || run.driver(case), |outcome| run.recorded(outcome))
    }

    fn policy(&mut self, suite: &Suite) {
        let Ok(run) = PolicyRun::new(suite) else {
            return;
        };
        let requests: Vec<EvaluateRequest> = (0..suite.cases.len() * 3).map(|i| PolicyRun::request(&suite.cases[i % suite.cases.len()])).collect();
        let batch = run.workspace.evaluate_batch(&requests);
        for (i, case) in suite.cases.iter().enumerate() {
            let want = Self::driver(&self.reference, &run, suite, case);
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
                self.policy_columns(suite, &run, &cases, goals, &targets, &built.columns());
            }
            for built in Built::nested(&rows).into_iter().chain(Built::nested(&cycled)) {
                let fields = built.fields();
                let structs = built.structs(&fields);
                self.nested += built.rows;
                self.policy_columns(suite, &run, &cases, goals, &targets, &built.columns_with(&structs));
            }
        }
    }

    fn policy_columns(&mut self, suite: &Suite, run: &PolicyRun, cases: &[&Case], goals: &[String], targets: &[Arc<str>], columns: &zen_expression::lane::Columns) {
        let Ok(output) = run.workspace.evaluate_columns(&Arc::from(PolicyRun::MAIN), targets, columns) else {
            return;
        };
        self.hosted += output.hosted;
        self.typed += output.typed;
        self.laned += output.laned;
        for row in 0..columns.rows {
            let input = columns.row(row);
            let request = EvaluateRequest {
                policy_path: Arc::from(PolicyRun::MAIN),
                input: input.depth_clone(64),
                goals: targets.to_vec(),
                trace: false,
            };
            let want = self.reference.policy(
                || Canon::key(&suite.id(), goals, &input),
                || Live::policy(&run.workspace, &request).map(|r| Built::normalized(r.output.to_value())).map_err(|e| format!("{e:?}")),
                |outcome| match outcome {
                    Outcome::Ok(value) => Ok(Built::normalized(Canon::restore(value).to_value())),
                    Outcome::Err { debug, .. } => Err(debug.clone()),
                },
            );
            let got = match &output.errors[row] {
                Some(error) => Err(Reference::debug(error)),
                None => Ok(Built::normalized(output.row(row).to_value())),
            };
            self.columnar += 1;
            if got != want && !self.reference.ordered(|| Canon::key(&suite.id(), goals, &input)) {
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
    if Reference::unavailable("spec-graph") {
        return;
    }
    Spec::prepare();
    let (suites, mut problems) = Spec::suites(Dialect::Graph);
    let reference = Reference::open("spec-graph");
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
                    out.push(Engines::walker(&reference, &run, suite, case).await);
                }
                out
            })
        });
        for (case, got) in suite.cases.iter().zip(outcomes.unwrap_or_default()) {
            report.record(suite, case, &got);
        }
    }
    report.failures.extend(reference.missing());
    report.finish("graph spec", problems);
}

#[test]
fn graph_engines_match_walker() {
    if Reference::unavailable("spec-graph") {
        return;
    }
    Spec::prepare();
    let (suites, problems) = Spec::suites(Dialect::Graph);
    let mut engines = Engines::new(Reference::open("spec-graph"));
    for suite in &suites {
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let mut failures = Vec::new();
        Guard::run(label, &mut failures, || Guard::block(engines.graph(suite)));
        engines.failures.extend(failures);
    }
    engines.failures.extend(engines.reference.missing());
    for (verdict, count) in &engines.census {
        eprintln!("{verdict}: {count} suites");
    }
    eprintln!(
        "graph engines: {} cases, {} columnar rows, {} mismatches",
        engines.compared,
        engines.columnar,
        engines.failures.len()
    );
    eprintln!("graph engines: {} typed units, {} laned units", engines.typed, engines.laned);
    assert!(std::env::var("ZEN_TYPED").is_ok_and(|v| v.trim() == "0") || engines.typed > 0, "no graph unit ran typed");
    Differential::assert(problems, engines.failures);
}

#[test]
fn policy_spec_matches_driver() {
    if Reference::unavailable("spec-policy") {
        return;
    }
    Spec::prepare();
    let (suites, mut problems) = Spec::suites(Dialect::Policy);
    let reference = Reference::open("spec-policy");
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
                .map(|case| Engines::driver(&reference, &run, suite, case))
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
    report.failures.extend(reference.missing());
    report.finish("policy spec", problems);
}

#[test]
fn policy_engines_match_driver() {
    if Reference::unavailable("spec-policy") {
        return;
    }
    Spec::prepare();
    let (suites, problems) = Spec::suites(Dialect::Policy);
    let mut engines = Engines::new(Reference::open("spec-policy"));
    for suite in &suites {
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let mut failures = Vec::new();
        Guard::run(label, &mut failures, || engines.policy(suite));
        engines.failures.extend(failures);
    }
    engines.failures.extend(engines.reference.missing());
    eprintln!("policy engines: {} cases, {} columnar rows ({} with struct lists, {} hosted per row), {} mismatches", engines.compared, engines.columnar, engines.nested, engines.hosted, engines.failures.len());
    eprintln!("policy engines: {} typed units, {} laned units", engines.typed, engines.laned);
    assert!(std::env::var("ZEN_TYPED").is_ok_and(|v| v.trim() == "0") || engines.typed > 0, "no policy unit ran typed");
    Differential::assert(problems, engines.failures);
}

struct Snapshot {
    lines: Vec<String>,
}

impl Snapshot {
    const CALLS: usize = 4;

    fn paths<'p>(paths: impl Iterator<Item = &'p Arc<str>>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for path in paths {
            path.hash(&mut hasher);
        }
        hasher.finish()
    }

    fn kinds(errors: impl Iterator<Item = String>) -> String {
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for error in errors {
            let kind: String = error.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            *kinds.entry(kind).or_default() += 1;
        }
        kinds.iter().map(|(kind, count)| format!("{kind}={count}")).collect::<Vec<_>>().join(",")
    }

    fn graph_rows(rows: &[Value]) -> Vec<(&'static str, Built)> {
        let cycled: Vec<Value> = (0..rows.len() * 3).map(|i| rows[i % rows.len()].clone()).collect();
        let mut builds = Vec::new();
        builds.extend(Built::new(rows).map(|b| ("flat", b)));
        builds.extend(Built::lists(rows).map(|b| ("lists", b)));
        builds.extend(Built::new(&cycled).map(|b| ("cycled", b)));
        builds.extend(Built::nested(rows).map(|b| ("nested", b)));
        builds.extend(Built::nested(&cycled).map(|b| ("nested-cycled", b)));
        builds
    }

    async fn graph(&mut self, suite: &Suite) {
        let rows: Vec<Value> = suite.cases.iter().map(|c| c.input.to_value()).collect();
        for (label, built) in Self::graph_rows(&rows) {
            let Ok(run) = GraphRun::new(suite) else {
                return;
            };
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            for call in 1..=Self::CALLS {
                let output = run.compiled.evaluate_columns(&columns, EvaluationOptions::default()).await;
                let errors = output.errors.iter().flatten().map(|e| format!("{e:?}"));
                self.lines.push(format!(
                    "graph\t{}\t{label}\tcall{call}\trows={}\ttyped={}\tlaned={}\terrors=[{}]\tpaths={:016x}",
                    suite.id(),
                    output.rows,
                    output.typed,
                    output.laned,
                    Self::kinds(errors),
                    Self::paths(output.columns.iter().map(|(path, _)| path))
                ));
            }
        }
    }

    fn policy(&mut self, suite: &Suite) {
        let Ok(probe) = PolicyRun::new(suite) else {
            return;
        };
        if !probe.blocking.is_empty() && probe.gate {
            return;
        }
        let mut goal_sets: Vec<&Vec<String>> = suite.cases.iter().map(|c| &c.goals).collect();
        goal_sets.dedup();
        for goals in goal_sets {
            let rows: Vec<Value> = suite.cases.iter().filter(|c| &c.goals == goals).map(|c| c.input.to_value()).collect();
            let targets: Vec<Arc<str>> = goals.iter().map(|g| Arc::from(g.as_str())).collect();
            for (label, built) in Self::graph_rows(&rows) {
                let Ok(run) = PolicyRun::new(suite) else {
                    return;
                };
                let fields = built.fields();
                let structs = built.structs(&fields);
                let columns = match label.starts_with("nested") {
                    true => built.columns_with(&structs),
                    false => built.columns(),
                };
                for call in 1..=Self::CALLS {
                    let line = match run.workspace.evaluate_columns(&Arc::from(PolicyRun::MAIN), &targets, &columns) {
                        Ok(output) => format!(
                            "rows={}\ttyped={}\tlaned={}\thosted={}\terrors=[{}]\tpaths={:016x}\torder={:016x}",
                            output.rows,
                            output.typed,
                            output.laned,
                            output.hosted,
                            Self::kinds(output.errors.iter().flatten().map(|e| format!("{e:?}"))),
                            Self::paths(output.columns.iter().map(|(path, _)| path).collect::<std::collections::BTreeSet<_>>().into_iter()),
                            Self::paths(output.columns.iter().map(|(path, _)| path))
                        ),
                        Err(error) => format!("refused {}", Self::kinds(std::iter::once(format!("{error:?}")))),
                    };
                    self.lines.push(format!("policy\t{}\t{goals:?}\t{label}\tcall{call}\t{line}", suite.id()));
                }
            }
        }
    }
}

#[test]
#[ignore]
fn consolidation_snapshot() {
    Spec::prepare();
    let mut snapshot = Snapshot { lines: Vec::new() };
    let (suites, _) = Spec::suites(Dialect::Graph);
    for suite in &suites {
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let mut failures = Vec::new();
        Guard::run(label, &mut failures, || Guard::block(snapshot.graph(suite)));
        snapshot.lines.extend(failures.iter().map(|f| format!("graph\tpanic\t{}", f.lines().next().unwrap_or_default())));
    }
    let (suites, _) = Spec::suites(Dialect::Policy);
    for suite in &suites {
        let label = format!("{}:{} [{}]", suite.file, suite.line, suite.name);
        let mut failures = Vec::new();
        Guard::run(label, &mut failures, || snapshot.policy(suite));
        snapshot.lines.extend(failures.iter().map(|f| format!("policy\tpanic\t{}", f.lines().next().unwrap_or_default())));
    }
    for line in &snapshot.lines {
        println!("SNAPSHOT\t{line}");
    }
}

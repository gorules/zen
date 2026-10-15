#[path = "support/policy_gen.rs"]
mod policy_gen;
#[allow(dead_code)]
mod conformance;
mod support;
#[path = "support/fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/policies.rs"]
mod policies;

use conformance::run::{GraphRun, Guard, PolicyRun};
use conformance::{Dialect, Spec, Suite};
use fixture::{Fixture, NETWORK_BOUND, TIME_BOUND};
use policies::{Anchor, Generated, PolicyCase};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use support::columns::Built;
#[cfg(feature = "driver")]
use support::golden::Live;
use support::golden::{Canon, Entry, Golden, Outcome};
use zen_engine::policy::{EvaluateRequest, PolicyWorkspace};
use zen_engine::{Decision, EvaluationError, EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind, ZEN_CONFIG};
use zen_expression::lane::Columns;
use zen_expression::Variable;

static GRAPH: Mutex<()> = Mutex::new(());

struct Nodes;

impl Nodes {
    const CHAPTER: &'static str = "15-nodes-references";

    fn modes(suite: &Suite) -> Vec<bool> {
        match suite.file.contains(Self::CHAPTER) {
            true => vec![true, false],
            false => vec![true],
        }
    }

    fn id(suite: &Suite, nodes: bool) -> String {
        match nodes {
            true => suite.id(),
            false => format!("{}#nodes-off", suite.id()),
        }
    }

    fn set(nodes: bool) {
        ZEN_CONFIG.nodes_in_context.store(nodes, Ordering::Relaxed);
    }

    fn lock() -> MutexGuard<'static, ()> {
        GRAPH.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct Rows;

impl Rows {
    fn cycled(rows: &[Value], times: usize) -> Vec<Value> {
        (0..rows.len() * times).map(|i| rows[i % rows.len()].clone()).collect()
    }

    fn spec_graph(rows: &[Value]) -> Vec<(Built, bool)> {
        let cycled = Self::cycled(rows, 3);
        Built::new(rows)
            .into_iter()
            .chain(Built::lists(rows))
            .chain(Built::new(&cycled))
            .chain(Built::nested(rows))
            .chain(Built::nested(&cycled))
            .map(|b| (b, true))
            .collect()
    }

    fn spec_policy(rows: &[Value]) -> Vec<(Built, bool)> {
        let cycled = Self::cycled(rows, 3);
        let flat = Built::new(rows).into_iter().chain(Built::lists(rows)).chain(Built::new(&cycled)).map(|b| (b, false));
        let nested = Built::nested(rows).into_iter().chain(Built::nested(&cycled)).map(|b| (b, true));
        flat.chain(nested).collect()
    }

    fn generated(inputs: &[Value]) -> Vec<(Built, bool)> {
        let flat = Built::new(inputs).into_iter().chain(Built::lists(inputs)).map(|b| (b, false));
        flat.chain(Built::nested(inputs).map(|b| (b, true))).collect()
    }

    fn fixture(variants: &[Value]) -> Vec<(Built, bool)> {
        let Some(first) = Built::new(variants) else {
            return Vec::new();
        };
        let cycled = Self::cycled(variants, 4);
        std::iter::once(first)
            .chain(Built::lists(variants))
            .chain(Built::new(&cycled))
            .chain(Built::nested(variants))
            .chain(Built::nested(&cycled))
            .map(|b| (b, true))
            .collect()
    }

    fn with<T>(built: &Built, structs: bool, f: impl FnOnce(&Columns) -> T) -> T {
        match structs {
            true => {
                let fields = built.fields();
                let parts = built.structs(&fields);
                f(&built.columns_with(&parts))
            }
            false => f(&built.columns()),
        }
    }

    fn of(builds: &[(Built, bool)]) -> Vec<Variable> {
        builds.iter().flat_map(|(built, structs)| Self::with(built, *structs, |c| (0..c.rows).map(|r| c.row(r)).collect::<Vec<_>>())).collect()
    }
}

struct Graphs;

impl Graphs {
    fn traced() -> EvaluationOptions {
        EvaluationOptions { trace: true, max_depth: 10 }
    }

    fn serialized() -> EvaluationSerializedOptions {
        EvaluationSerializedOptions {
            trace: EvaluationTraceKind::Reference,
            max_depth: 10,
        }
    }

    #[cfg(feature = "driver")]
    async fn record(golden: &mut Golden, id: &str, walker: &Decision, input: &Variable, traced: bool) {
        let plain = Outcome::graph(&walker.evaluate(input.depth_clone(64)).await);
        let entry = Entry::new(id, &[], input, plain);
        let entry = match traced {
            true => {
                let traced = Outcome::graph_traced(&walker.evaluate_with_opts(input.depth_clone(64), Self::traced()).await, EvaluationTraceKind::Default);
                let reference = Canon::reference(&traced, walker.evaluate_serialized(input.depth_clone(64), Self::serialized()).await);
                entry.traced(traced, Some(reference))
            }
            false => entry,
        };
        golden.insert(entry);
    }

    async fn verify(check: &mut Check, golden: &Golden, id: &str, decision: &Decision, input: &Variable, label: &str) {
        let key = Canon::key(id, &[], input);
        let Some(entry) = check.entry(golden, &key) else {
            return;
        };
        let plain = Outcome::graph(&decision.evaluate(input.depth_clone(64)).await);
        check.same(&key, &format!("{label} single"), &entry.plain, &plain);
        if let Some(traced) = &entry.traced {
            let got = Outcome::graph_traced(&decision.evaluate_with_opts(input.depth_clone(64), Self::traced()).await, EvaluationTraceKind::Default);
            check.same(&key, &format!("{label} traced"), traced, &got);
            let reference = Canon::reference(traced, decision.evaluate_serialized(input.depth_clone(64), Self::serialized()).await);
            check.values(&key, &format!("{label} reference"), entry.reference.as_ref(), Some(&reference));
        }
    }

    async fn columns(check: &mut Check, golden: &Golden, id: &str, decision: &Decision, builds: &[(Built, bool)]) {
        for (built, _) in builds {
            let fields = built.fields();
            let structs = built.structs(&fields);
            let columns = built.columns_with(&structs);
            let store = zen_engine::ColumnStore::new();
            let output = decision.evaluate_columns(&columns, &store, EvaluationOptions::default()).await;
            for row in 0..columns.rows {
                let input = columns.row(row);
                let key = Canon::key(id, &[], &input);
                let Some(entry) = check.entry(golden, &key) else {
                    continue;
                };
                let got = match &output.errors[row] {
                    Some(error) => Outcome::Err {
                        debug: error.to_string(),
                        value: Canon::value(serde_json::to_value(&**error).unwrap_or(Value::Null)),
                    },
                    None => Outcome::Ok(Canon::variable(&output.row(row))),
                };
                check.same(&key, "columns", &entry.plain.normalized(Built::normalized), &got.normalized(Built::normalized));
            }
        }
    }

    async fn batch(check: &mut Check, golden: &Golden, id: &str, decision: &Decision, inputs: &[Variable], label: &str) {
        let keys: Vec<String> = inputs.iter().map(|input| Canon::key(id, &[], input)).collect();
        let results = decision.evaluate_batch(inputs, EvaluationOptions::default()).await;
        for (key, result) in keys.into_iter().zip(&results) {
            if let Some(entry) = check.entry(golden, &key) {
                check.same(&key, label, &entry.plain, &Outcome::graph(result));
            }
        }
    }
}

#[cfg(feature = "driver")]
struct Modes;

#[cfg(feature = "driver")]
impl Modes {
    const ALL: [EvaluationTraceKind; 4] = [EvaluationTraceKind::Default, EvaluationTraceKind::String, EvaluationTraceKind::Reference, EvaluationTraceKind::ReferenceString];

    async fn compare(check: &mut Check, live: &Golden, id: &str, walker: &Decision, decision: &Decision, input: &Variable, label: &str) {
        let key = Canon::key(id, &[], input);
        if check.entry(live, &key).and_then(|entry| entry.traced.as_ref()).is_none() {
            return;
        }
        for mode in Self::ALL {
            let options = EvaluationSerializedOptions { trace: mode, max_depth: 10 };
            let reference = matches!(mode, EvaluationTraceKind::Reference | EvaluationTraceKind::ReferenceString);
            let want = Canon::mode(walker.evaluate_serialized(input.depth_clone(64), options).await, reference);
            let got = Canon::mode(decision.evaluate_serialized(input.depth_clone(64), options).await, reference);
            check.same(&key, &format!("{label} {mode:?}"), &want, &got);
        }
    }
}

struct Policies;

impl Policies {
    fn request(path: &str, input: &Variable, goals: &[String], trace: bool) -> EvaluateRequest {
        EvaluateRequest {
            policy_path: Arc::from(path),
            input: input.depth_clone(64),
            goals: goals.iter().map(|g| Arc::from(g.as_str())).collect(),
            trace,
        }
    }

    #[cfg(feature = "driver")]
    fn record(golden: &mut Golden, ws: &PolicyWorkspace, id: &str, path: &str, input: &Variable, goals: &[String], traced: bool, fingerprint: Option<String>) {
        let plain = Outcome::policy(Live::policy(ws, &Self::request(path, input, goals, false)));
        let entry = Entry::new(id, goals, input, plain).fingerprint(fingerprint);
        let entry = match traced {
            true => entry
                .traced(Outcome::policy_traced(Live::policy(ws, &Self::request(path, input, goals, true))), None)
                .enhanced(Outcome::policy_traced(Live::enhanced(ws, &Self::request(path, input, goals, false)))),
            false => entry,
        };
        golden.insert(entry);
    }

    fn verify(check: &mut Check, golden: &Golden, ws: &PolicyWorkspace, id: &str, path: &str, input: &Variable, goals: &[String], fingerprint: Option<&str>) {
        let key = Canon::key(id, goals, input);
        let Some(entry) = check.entry(golden, &key) else {
            return;
        };
        if fingerprint.is_some() && entry.fingerprint.as_deref() != fingerprint {
            check.failures.push(format!("{key}: fingerprint changed, goldens are stale"));
        }
        check.same(&key, "single", &entry.plain, &Outcome::policy(ws.evaluate(&Self::request(path, input, goals, false))));
        if let Some(traced) = &entry.traced {
            check.same(&key, "traced", traced, &Outcome::policy_traced(ws.evaluate(&Self::request(path, input, goals, true))));
        }
        if let Some(enhanced) = &entry.enhanced {
            check.same(&key, "enhanced", enhanced, &Outcome::policy_traced(ws.enhance_trace(&Self::request(path, input, goals, false))));
        }
    }

    fn batch(check: &mut Check, golden: &Golden, ws: &PolicyWorkspace, id: &str, path: &str, requests: &[(Variable, Vec<String>)]) {
        let batch: Vec<EvaluateRequest> = requests.iter().map(|(input, goals)| Self::request(path, input, goals, false)).collect();
        for ((input, goals), result) in requests.iter().zip(ws.evaluate_batch(&batch)) {
            let key = Canon::key(id, goals, input);
            if let Some(entry) = check.entry(golden, &key) {
                check.same(&key, "batch", &entry.plain, &Outcome::policy(result));
            }
        }
    }

    fn columns(check: &mut Check, golden: &Golden, ws: &PolicyWorkspace, id: &str, path: &str, goals: &[String], builds: &[(Built, bool)]) {
        let targets: Vec<Arc<str>> = goals.iter().map(|g| Arc::from(g.as_str())).collect();
        for (built, structs) in builds {
            Rows::with(built, *structs, |columns| {
                let store = zen_engine::ColumnStore::new();
                let output = ws.evaluate_columns(&Arc::from(path), &targets, columns, &store);
                for row in 0..columns.rows {
                    let input = columns.row(row);
                    let key = Canon::key(id, goals, &input);
                    let Some(entry) = check.entry(golden, &key) else {
                        continue;
                    };
                    let error = match &output {
                        Ok(output) => output.errors[row].as_ref(),
                        Err(refused) => Some(refused),
                    };
                    let got = match (error, &output) {
                        (Some(error), _) => Outcome::Err {
                            debug: Outcome::debug(error),
                            value: Value::Null,
                        },
                        (None, Ok(output)) => Outcome::Ok(Canon::variable(&output.row(row))),
                        (None, Err(_)) => Outcome::missing(&key),
                    };
                    let want = match &entry.plain {
                        Outcome::Err { debug, .. } => Outcome::Err {
                            debug: debug.clone(),
                            value: Value::Null,
                        },
                        ok => ok.normalized(Built::normalized),
                    };
                    check.same(&key, "columns", &want, &got.normalized(Built::normalized));
                }
            });
        }
    }
}

#[derive(Default)]
struct Check {
    failures: Vec<String>,
    compared: usize,
    ordered: usize,
    divergent: BTreeSet<String>,
    skipped: usize,
}

impl Check {
    fn entry<'g>(&mut self, golden: &'g Golden, key: &str) -> Option<&'g Entry> {
        match golden.get(key) {
            None => {
                self.failures.push(format!("{key}: missing golden entry"));
                None
            }
            Some(entry) if entry.ordered => {
                self.ordered += 1;
                None
            }
            entry => entry,
        }
    }

    fn diverges(&mut self, suite: &Suite, id: &str, inputs: impl IntoIterator<Item = Variable>) {
        for input in inputs {
            let value = Built::normalized(input.to_value());
            let case = suite.cases.iter().find(|c| Built::normalized(c.input.to_value()) == value).unwrap_or(&suite.cases[0]);
            if case.engines.is_some() {
                self.divergent.insert(Canon::key(id, &[], &input));
            }
        }
    }

    fn same(&mut self, key: &str, path: &str, want: &Outcome, got: &Outcome) {
        if self.divergent.contains(key) {
            self.skipped += 1;
            return;
        }
        self.compared += 1;
        if want != got {
            self.failures.push(format!("{key} {path}\n  golden {}\n  got    {}", want.show(), got.show()));
        }
    }

    fn values(&mut self, key: &str, path: &str, want: Option<&Value>, got: Option<&Value>) {
        self.compared += 1;
        if want != got {
            self.failures.push(format!("{key} {path}\n  golden {want:?}\n  got    {got:?}"));
        }
    }

    fn finish(mut self, golden: &Golden, label: &str) {
        let unvisited = golden.unvisited();
        if !unvisited.is_empty() {
            self.failures.push(format!("{} golden entries never checked, e.g. {:?}", unvisited.len(), unvisited.iter().take(5).collect::<Vec<_>>()));
        }
        eprintln!("{label}: {} entries, {} comparisons, {} key-order dependent lookups and {} documented engine divergences skipped, {} failures", golden.len(), self.compared, self.ordered, self.skipped, self.failures.len());
        let limit = std::env::var("SPEC_SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
        assert!(self.failures.is_empty(), "{} failures:\n{}", self.failures.len(), self.failures.iter().take(limit).cloned().collect::<Vec<_>>().join("\n"));
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Corpus {
    SpecGraph,
    SpecPolicy,
    Generated,
    PolicyFixtures,
    Anchor,
    GraphFixtures,
}

impl Corpus {
    #[cfg(feature = "driver")]
    const ALL: [Corpus; 6] = [Corpus::SpecGraph, Corpus::SpecPolicy, Corpus::Generated, Corpus::PolicyFixtures, Corpus::Anchor, Corpus::GraphFixtures];

    fn name(&self) -> &'static str {
        match self {
            Corpus::SpecGraph => "spec-graph",
            Corpus::SpecPolicy => "spec-policy",
            Corpus::Generated => "generated/policy",
            Corpus::PolicyFixtures => "policy-fixtures",
            Corpus::Anchor => "anchor",
            Corpus::GraphFixtures => "graph-fixtures",
        }
    }

    #[cfg(feature = "driver")]
    fn shard(&self, entry: &Entry) -> Option<String> {
        match self {
            Corpus::SpecGraph | Corpus::SpecPolicy => {
                let file = entry.suite.split('#').next().unwrap_or_default();
                let name = file.rsplit('/').next().unwrap_or(file);
                Some(name.trim_end_matches(".json5").to_string())
            }
            _ => None,
        }
    }

    #[cfg(feature = "driver")]
    fn selected() -> Vec<Corpus> {
        let only = std::env::var("GOLDEN_CORPUS").ok();
        Self::ALL.into_iter().filter(|c| only.as_ref().is_none_or(|o| c.name().contains(o.as_str()))).collect()
    }

    #[cfg(feature = "driver")]
    fn record(&self) -> Golden {
        Golden::prepare();
        let mut golden = Golden::new(self.name());
        let mut failures = Vec::new();
        match self {
            Corpus::SpecGraph => Self::spec_graph(&mut golden, &mut failures),
            Corpus::SpecPolicy => Self::spec_policy(&mut golden, &mut failures),
            Corpus::Generated => Self::generated(&mut golden),
            Corpus::PolicyFixtures => Self::policy_fixtures(&mut golden),
            Corpus::Anchor => Self::anchor(&mut golden),
            Corpus::GraphFixtures => Guard::block(Self::graph_fixtures(&mut golden)),
        }
        failures.extend(golden.conflicts.iter().cloned());
        assert!(failures.is_empty(), "{}: recording failed:\n{}", self.name(), failures.join("\n"));
        golden
    }

    #[cfg(feature = "driver")]
    fn spec_graph(golden: &mut Golden, failures: &mut Vec<String>) {
        let (suites, problems) = Spec::suites(Dialect::Graph);
        failures.extend(problems);
        let _guard = Nodes::lock();
        for suite in &suites {
            for nodes in Nodes::modes(suite) {
                Nodes::set(nodes);
                let Ok(run) = GraphRun::new(suite) else {
                    continue;
                };
                let id = Nodes::id(suite, nodes);
                let rows: Vec<Value> = suite.cases.iter().map(|c| c.input.to_value()).collect();
                let label = format!("{id} record");
                Guard::run(label, failures, || {
                    Guard::block(async {
                        for case in &suite.cases {
                            Graphs::record(golden, &id, &run.walker, &case.input, true).await;
                        }
                        for row in Rows::of(&Rows::spec_graph(&rows)) {
                            Graphs::record(golden, &id, &run.walker, &row, false).await;
                        }
                    })
                });
            }
        }
        Nodes::set(true);
    }

    #[cfg(feature = "driver")]
    fn spec_policy(golden: &mut Golden, failures: &mut Vec<String>) {
        let (suites, problems) = Spec::suites(Dialect::Policy);
        failures.extend(problems);
        for suite in &suites {
            let Ok(run) = PolicyRun::new(suite) else {
                continue;
            };
            let id = suite.id();
            let label = format!("{id} record");
            Guard::run(label, failures, || {
                golden.insert(Entry::new(&format!("{id}#diagnostics"), &[], &Variable::Null, Outcome::Ok(json!(run.blocking))));
                for case in &suite.cases {
                    Policies::record(golden, &run.workspace, &id, PolicyRun::MAIN, &case.input, &case.goals, true, None);
                }
                if !run.blocking.is_empty() && run.gate {
                    return;
                }
                for (goals, rows) in Self::goal_rows(suite) {
                    for row in Rows::of(&Rows::spec_policy(&rows)) {
                        Policies::record(golden, &run.workspace, &id, PolicyRun::MAIN, &row, &goals, false, None);
                    }
                }
            });
        }
    }

    fn goal_rows(suite: &Suite) -> Vec<(Vec<String>, Vec<Value>)> {
        let mut goal_sets: Vec<&Vec<String>> = suite.cases.iter().map(|c| &c.goals).collect();
        goal_sets.dedup();
        goal_sets
            .into_iter()
            .map(|goals| (goals.clone(), suite.cases.iter().filter(|c| &c.goals == goals).map(|c| c.input.to_value()).collect()))
            .collect()
    }

    #[cfg(feature = "driver")]
    fn computed(ws: &PolicyWorkspace, path: &str, inputs: &[Value]) -> Vec<String> {
        inputs
            .iter()
            .find_map(|input| Live::policy(ws, &Policies::request(path, &Variable::from(input.clone()), &[], false)).ok().map(|r| PolicyCase::computed(&r.output.to_value(), input)))
            .unwrap_or_default()
    }

    fn goal_sets(computed: &[String], i: usize) -> Vec<Vec<String>> {
        let goals = PolicyCase::goals(computed, i);
        match goals.is_empty() {
            true => vec![goals],
            false => vec![Vec::new(), goals],
        }
    }

    #[cfg(feature = "driver")]
    fn generated(golden: &mut Golden) {
        for seed in 0..Generated::seeds() {
            let Some(ws) = Generated::workspace(seed) else {
                continue;
            };
            let (inputs, label, fingerprint) = (Generated::inputs(seed), Generated::label(seed), Generated::fingerprint(seed));
            let computed = Self::computed(&ws, Generated::PATH, &inputs);
            for (i, input) in inputs.iter().enumerate() {
                for goals in Self::goal_sets(&computed, i) {
                    Policies::record(golden, &ws, &label, Generated::PATH, &Variable::from(input.clone()), &goals, i < 8, Some(fingerprint.clone()));
                }
            }
            for row in Rows::of(&Rows::generated(&inputs)) {
                Policies::record(golden, &ws, &label, Generated::PATH, &row, &[], false, Some(fingerprint.clone()));
            }
        }
    }

    #[cfg(feature = "driver")]
    fn policy_fixtures(golden: &mut Golden) {
        for case in PolicyCase::load_all() {
            let computed = Self::computed(&case.workspace, &case.path, &case.inputs);
            for (i, input) in case.inputs.iter().enumerate() {
                for goals in Self::goal_sets(&computed, i) {
                    Policies::record(golden, &case.workspace, &case.name, &case.path, &Variable::from(input.clone()), &goals, false, None);
                }
            }
        }
    }

    #[cfg(feature = "driver")]
    fn anchor(golden: &mut Golden) {
        for batch in Anchor::batches() {
            let ws = batch.workspace();
            for row in batch.column_rows() {
                Policies::record(golden, &ws, batch.suite, Anchor::PATH, &row, &[], false, None);
            }
        }
        let base = Anchor::base();
        let ws = base.workspace();
        for (at, value) in base.values.iter().enumerate() {
            Policies::record(golden, &ws, base.suite, Anchor::PATH, &Variable::from(value.clone()), &[], at < 32, None);
        }
    }

    fn fixtures() -> Vec<Fixture> {
        Fixture::load_all()
            .into_iter()
            .filter(|f| !NETWORK_BOUND.contains(&f.name.as_str()))
            .collect()
    }

    fn slow(fixture: &Fixture) -> bool {
        TIME_BOUND.contains(&fixture.name.as_str())
    }

    fn fixture_inputs(fixture: &Fixture) -> Vec<Variable> {
        fixture.variants().into_iter().chain(fixture.typed_variants()).map(Variable::from).collect()
    }

    fn compiled(fixture: &Fixture) -> Decision {
        let mut compiled = Decision::from(Arc::new(fixture.content.clone()));
        compiled.compile();
        compiled
    }

    #[cfg(feature = "driver")]
    async fn graph_fixtures(golden: &mut Golden) {
        let _guard = Nodes::lock();
        for fixture in Self::fixtures() {
            let walker = Decision::from(Arc::new(fixture.content.clone())).interpreted();
            for input in Self::fixture_inputs(&fixture) {
                Graphs::record(golden, &fixture.name, &walker, &input, !Self::slow(&fixture)).await;
            }
            if !Self::slow(&fixture) && matches!(Self::compiled(&fixture).compiled_verdict(), Some(Ok(()))) {
                for row in Rows::of(&Rows::fixture(&fixture.variants())) {
                    Graphs::record(golden, &fixture.name, &walker, &row, false).await;
                }
            }
        }
    }

    #[cfg(feature = "driver")]
    fn modes(&self, live: &Golden) {
        let mut check = Check::default();
        match self {
            Corpus::SpecGraph => Self::modes_spec_graph(live, &mut check),
            Corpus::GraphFixtures => Guard::block(Self::modes_graph_fixtures(live, &mut check)),
            _ => {}
        }
        check.finish(live, &format!("{} trace modes", self.name()));
    }

    #[cfg(feature = "driver")]
    fn modes_spec_graph(live: &Golden, check: &mut Check) {
        let (suites, _) = Spec::suites(Dialect::Graph);
        let _guard = Nodes::lock();
        for suite in &suites {
            for nodes in Nodes::modes(suite) {
                Nodes::set(nodes);
                let Ok(run) = GraphRun::new(suite) else {
                    continue;
                };
                let id = Nodes::id(suite, nodes);
                check.diverges(suite, &id, suite.cases.iter().map(|c| c.input.clone()));
                let mut failures = Vec::new();
                Guard::run(format!("{id} trace modes"), &mut failures, || {
                    Guard::block(async {
                        for case in &suite.cases {
                            Modes::compare(check, live, &id, &run.walker, &run.compiled, &case.input, "compiled").await;
                            Modes::compare(check, live, &id, &run.walker, &run.uncompiled, &case.input, "uncompiled").await;
                        }
                    })
                });
                check.failures.extend(failures);
            }
        }
        Nodes::set(true);
    }

    #[cfg(feature = "driver")]
    async fn modes_graph_fixtures(live: &Golden, check: &mut Check) {
        let _guard = Nodes::lock();
        for fixture in Self::fixtures().into_iter().filter(|f| !Self::slow(f)) {
            let walker = Decision::from(Arc::new(fixture.content.clone())).interpreted();
            let compiled = Self::compiled(&fixture);
            let uncompiled = Decision::from(Arc::new(fixture.content.clone()));
            for input in Self::fixture_inputs(&fixture) {
                Modes::compare(check, live, &fixture.name, &walker, &compiled, &input, "compiled").await;
                Modes::compare(check, live, &fixture.name, &walker, &uncompiled, &input, "uncompiled").await;
            }
        }
    }

    fn verify(&self) {
        Golden::prepare();
        self.verify_against(&Golden::open(self.name()));
    }

    fn verify_against(&self, golden: &Golden) {
        let mut check = Check::default();
        match self {
            Corpus::SpecGraph => Self::verify_spec_graph(golden, &mut check),
            Corpus::SpecPolicy => Self::verify_spec_policy(golden, &mut check),
            Corpus::Generated => Self::verify_generated(golden, &mut check),
            Corpus::PolicyFixtures => Self::verify_policy_fixtures(golden, &mut check),
            Corpus::Anchor => Self::verify_anchor(golden, &mut check),
            Corpus::GraphFixtures => Guard::block(Self::verify_graph_fixtures(golden, &mut check)),
        }
        check.finish(golden, self.name());
    }

    fn verify_spec_graph(golden: &Golden, check: &mut Check) {
        let (suites, _) = Spec::suites(Dialect::Graph);
        let _guard = Nodes::lock();
        for suite in &suites {
            for nodes in Nodes::modes(suite) {
                Nodes::set(nodes);
                let Ok(run) = GraphRun::new(suite) else {
                    continue;
                };
                let id = Nodes::id(suite, nodes);
                let rows: Vec<Value> = suite.cases.iter().map(|c| c.input.to_value()).collect();
                let builds = Rows::spec_graph(&rows);
                check.diverges(suite, &id, suite.cases.iter().map(|c| c.input.clone()).chain(Rows::of(&builds)));
                let mut failures = Vec::new();
                Guard::run(format!("{id} verify"), &mut failures, || {
                    Guard::block(async {
                        for case in &suite.cases {
                            Graphs::verify(check, golden, &id, &run.compiled, &case.input, "compiled").await;
                            Graphs::verify(check, golden, &id, &run.uncompiled, &case.input, "uncompiled").await;
                            let key = Canon::key(&id, &[], &case.input);
                            if let Some(entry) = case.engines.is_none().then(|| check.entry(golden, &key)).flatten() {
                                let engine = Outcome::graph(&run.engine.evaluate(GraphRun::MAIN, case.input.depth_clone(64)).await);
                                check.same(&key, "engine", &entry.plain, &engine);
                                if let Some(traced) = &entry.traced {
                                    let got = Outcome::graph_traced(&run.engine.evaluate_with_opts(GraphRun::MAIN, case.input.depth_clone(64), Graphs::traced()).await, EvaluationTraceKind::Default);
                                    check.same(&key, "engine traced", traced, &got);
                                }
                            }
                        }
                        let contexts: Vec<Variable> = suite.cases.iter().cycle().take(suite.cases.len() * 3).map(|c| c.input.depth_clone(64)).collect();
                        Graphs::batch(check, golden, &id, &run.compiled, &contexts, "batch").await;
                        Graphs::columns(check, golden, &id, &run.compiled, &builds).await;
                    })
                });
                check.failures.extend(failures);
            }
        }
        Nodes::set(true);
    }

    fn verify_spec_policy(golden: &Golden, check: &mut Check) {
        let (suites, _) = Spec::suites(Dialect::Policy);
        for suite in &suites {
            let Ok(run) = PolicyRun::new(suite) else {
                continue;
            };
            let id = suite.id();
            let mut failures = Vec::new();
            Guard::run(format!("{id} verify"), &mut failures, || {
                let key = Canon::key(&format!("{id}#diagnostics"), &[], &Variable::Null);
                if let Some(entry) = check.entry(golden, &key) {
                    check.same(&key, "diagnostics", &entry.plain, &Outcome::Ok(json!(run.blocking)));
                }
                for case in &suite.cases {
                    Policies::verify(check, golden, &run.workspace, &id, PolicyRun::MAIN, &case.input, &case.goals, None);
                    let key = Canon::key(&id, &case.goals, &case.input);
                    if case.goals.is_empty() && run.gate && run.blocking.is_empty() && case.engines.is_none() {
                        if let Some(entry) = check.entry(golden, &key) {
                            let engine = match Guard::block(run.engine.evaluate(PolicyRun::MAIN, case.input.depth_clone(64))) {
                                Ok(response) => Outcome::Ok(Canon::variable(&response.result)),
                                Err(error) => match *error {
                                    EvaluationError::Policy(error) => Outcome::policy(Err(error)),
                                    other => Outcome::Err {
                                        debug: other.to_string(),
                                        value: Value::Null,
                                    },
                                },
                            };
                            check.same(&key, "engine", &entry.plain, &engine);
                        }
                    }
                }
                let requests: Vec<(Variable, Vec<String>)> = suite.cases.iter().cycle().take(suite.cases.len() * 3).map(|c| (c.input.clone(), c.goals.clone())).collect();
                Policies::batch(check, golden, &run.workspace, &id, PolicyRun::MAIN, &requests);
                if !run.blocking.is_empty() && run.gate {
                    return;
                }
                for (goals, rows) in Self::goal_rows(suite) {
                    Policies::columns(check, golden, &run.workspace, &id, PolicyRun::MAIN, &goals, &Rows::spec_policy(&rows));
                }
            });
            check.failures.extend(failures);
        }
    }

    fn goals_from(golden: &Golden, id: &str, inputs: &[Value]) -> Vec<String> {
        inputs
            .iter()
            .find_map(|input| match golden.get(&Canon::key(id, &[], &Variable::from(input.clone()))).map(|e| &e.plain) {
                Some(Outcome::Ok(output)) => Some(PolicyCase::computed(output, input)),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn verify_generated(golden: &Golden, check: &mut Check) {
        for seed in 0..Generated::seeds() {
            let Some(ws) = Generated::workspace(seed) else {
                continue;
            };
            let (inputs, label, fingerprint) = (Generated::inputs(seed), Generated::label(seed), Generated::fingerprint(seed));
            let computed = Self::goals_from(golden, &label, &inputs);
            let mut requests = Vec::new();
            for (i, input) in inputs.iter().enumerate() {
                for goals in Self::goal_sets(&computed, i) {
                    let variable = Variable::from(input.clone());
                    Policies::verify(check, golden, &ws, &label, Generated::PATH, &variable, &goals, Some(&fingerprint));
                    requests.push((variable, goals));
                }
            }
            Policies::batch(check, golden, &ws, &label, Generated::PATH, &requests);
            Policies::columns(check, golden, &ws, &label, Generated::PATH, &[], &Rows::generated(&inputs));
        }
    }

    fn verify_policy_fixtures(golden: &Golden, check: &mut Check) {
        for case in PolicyCase::load_all() {
            let computed = Self::goals_from(golden, &case.name, &case.inputs);
            let mut requests = Vec::new();
            for (i, input) in case.inputs.iter().enumerate() {
                for goals in Self::goal_sets(&computed, i) {
                    let variable = Variable::from(input.clone());
                    Policies::verify(check, golden, &case.workspace, &case.name, &case.path, &variable, &goals, None);
                    requests.push((variable, goals));
                }
            }
            Policies::batch(check, golden, &case.workspace, &case.name, &case.path, &requests);
        }
    }

    fn verify_anchor(golden: &Golden, check: &mut Check) {
        for batch in Anchor::batches() {
            let ws = batch.workspace();
            let rows = batch.column_rows();
            let builds: Vec<(Built, bool)> = Built::new(&batch.values).map(|b| (b, false)).into_iter().chain(Built::nested(&batch.values).map(|b| (b, true))).collect();
            Policies::columns(check, golden, &ws, batch.suite, Anchor::PATH, &[], &builds);
            for row in &rows {
                Policies::verify(check, golden, &ws, batch.suite, Anchor::PATH, row, &[], None);
            }
            let requests: Vec<(Variable, Vec<String>)> = rows.into_iter().map(|r| (r, Vec::new())).collect();
            Policies::batch(check, golden, &ws, batch.suite, Anchor::PATH, &requests);
        }
        let base = Anchor::base();
        let ws = base.workspace();
        for value in &base.values {
            Policies::verify(check, golden, &ws, base.suite, Anchor::PATH, &Variable::from(value.clone()), &[], None);
        }
    }

    async fn verify_graph_fixtures(golden: &Golden, check: &mut Check) {
        let _guard = Nodes::lock();
        for fixture in Self::fixtures() {
            let compiled = Self::compiled(&fixture);
            let uncompiled = Decision::from(Arc::new(fixture.content.clone()));
            let inputs = Self::fixture_inputs(&fixture);
            for input in &inputs {
                Graphs::verify(check, golden, &fixture.name, &compiled, input, "compiled").await;
                if !Self::slow(&fixture) {
                    Graphs::verify(check, golden, &fixture.name, &uncompiled, input, "uncompiled").await;
                }
            }
            if Self::slow(&fixture) {
                continue;
            }
            Graphs::batch(check, golden, &fixture.name, &compiled, &inputs, "batch").await;
            if matches!(compiled.compiled_verdict(), Some(Ok(()))) {
                Graphs::columns(check, golden, &fixture.name, &compiled, &Rows::fixture(&fixture.variants())).await;
            }
        }
    }
}

#[cfg(feature = "driver")]
#[test]
#[ignore]
fn capture() {
    assert!(std::env::var("GOLDEN_WRITE").is_ok_and(|v| v == "1"), "set GOLDEN_WRITE=1 to rewrite the goldens");
    for corpus in Corpus::selected() {
        let started = std::time::Instant::now();
        let golden = corpus.record();
        let written = golden.write(|entry| corpus.shard(entry));
        let (raw, stored) = written.iter().fold((0u64, 0u64), |(r, s), w| (r + w.raw, s + w.stored));
        let traced = golden.entries().filter(|e| e.traced.is_some()).count();
        let ordered: Vec<&str> = golden.entries().filter(|e| e.ordered).map(|e| e.suite.as_str()).collect();
        if !ordered.is_empty() {
            eprintln!("{}: {} entries depend on input key order: {:?}", corpus.name(), ordered.len(), ordered);
        }
        let enhanced = golden.entries().filter(|e| e.enhanced.is_some()).count();
        eprintln!(
            "{:18} {:>7} entries ({traced} traced, {enhanced} enhanced) {:>3} files raw {:>11} stored {:>11} in {:.1}s",
            corpus.name(),
            golden.len(),
            written.len(),
            raw,
            stored,
            started.elapsed().as_secs_f64()
        );
    }
}

#[cfg(feature = "driver")]
#[test]
fn goldens_are_current() {
    let mut failures = Vec::new();
    for corpus in Corpus::selected() {
        let live = corpus.record();
        let recorded = Golden::open(corpus.name());
        failures.extend(live.compare(&recorded));
    }
    let limit = std::env::var("SPEC_SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    assert!(failures.is_empty(), "{} stale golden entries:\n{}", failures.len(), failures.iter().take(limit).cloned().collect::<Vec<_>>().join("\n"));
}

#[cfg(feature = "driver")]
#[test]
fn hosts_match_the_live_driver() {
    for corpus in Corpus::selected().into_iter().filter(|c| !matches!(c, Corpus::SpecGraph | Corpus::GraphFixtures)) {
        let live = corpus.record();
        corpus.verify_against(&live);
    }
}

#[cfg(feature = "driver")]
#[test]
fn traced_graphs_match_the_live_walker() {
    for corpus in Corpus::selected().into_iter().filter(|c| matches!(c, Corpus::SpecGraph | Corpus::GraphFixtures)) {
        let live = corpus.record();
        corpus.verify_against(&live);
        corpus.modes(&live);
    }
}

#[test]
fn goldens_spec_graph() {
    Corpus::SpecGraph.verify();
}

#[test]
fn goldens_spec_policy() {
    Corpus::SpecPolicy.verify();
}

#[test]
fn goldens_generated_policy() {
    Corpus::Generated.verify();
}

#[test]
fn goldens_policy_fixtures() {
    Corpus::PolicyFixtures.verify();
}

#[test]
fn goldens_anchor() {
    Corpus::Anchor.verify();
}

#[test]
fn goldens_graph_fixtures() {
    Corpus::GraphFixtures.verify();
}

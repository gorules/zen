#[path = "support/policy_gen.rs"]
mod policy_gen;
mod conformance;
mod support;
#[allow(dead_code)]
#[path = "support/fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/policies.rs"]
mod policies;

use conformance::run::PolicyRun;
use conformance::{Dialect, Spec};
use fixture::Fixture;
use policies::{Anchor, Generated, PolicyCase};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use zen_engine::model::{DecisionContent, DecisionNodeKind, GraphContent};
use zen_engine::policy::{BlockDoc, PolicyDocument, PolicyWorkspace};
use zen_expression::lane::{Fallbacks, LaneProgram, Programs, Session};
use zen_expression::{ExpressionKind, Isolate, IsolateError, Variable};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Site {
    Standard(String),
    Cell { field: String, cell: String },
}

struct Document {
    label: String,
    sites: Vec<Site>,
    inputs: Vec<Variable>,
}

impl Document {
    const INPUTS: usize = 24;

    fn new(label: String, inputs: Vec<Variable>) -> Self {
        Self {
            label,
            sites: Vec::new(),
            inputs: inputs.into_iter().take(Self::INPUTS).collect(),
        }
    }

    fn standard(&mut self, source: &str) {
        if !source.is_empty() {
            self.sites.push(Site::Standard(source.to_string()));
        }
    }

    fn cell(&mut self, field: Option<&str>, cell: &str) {
        if cell.is_empty() {
            return;
        }
        match field {
            Some(field) => self.sites.push(Site::Cell {
                field: field.to_string(),
                cell: cell.to_string(),
            }),
            None => self.standard(cell),
        }
    }

    fn graph(&mut self, graph: &GraphContent) {
        for node in &graph.nodes {
            match &node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    self.transform(content.transform_attributes.input_field.as_deref());
                    for expression in content.expressions.iter() {
                        if !expression.key.is_empty() {
                            self.standard(&expression.value);
                        }
                    }
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    self.transform(content.transform_attributes.input_field.as_deref());
                    for input in content.inputs.iter() {
                        let field = input.field.as_deref().filter(|f| !f.trim().is_empty());
                        if let Some(field) = field {
                            self.standard(field);
                        }
                        for rule in content.rules.iter() {
                            if let Some(cell) = rule.get(&input.id) {
                                self.cell(field, cell);
                            }
                        }
                    }
                    for output in content.outputs.iter() {
                        for rule in content.rules.iter() {
                            if let (false, Some(cell)) = (output.field.is_empty(), rule.get(&output.id)) {
                                self.standard(cell);
                            }
                        }
                    }
                }
                DecisionNodeKind::SwitchNode { content } => {
                    for statement in content.statements.iter() {
                        self.standard(&statement.condition);
                    }
                }
                DecisionNodeKind::DecisionNode { content } => {
                    self.transform(content.transform_attributes.input_field.as_deref());
                }
                _ => {}
            }
        }
    }

    fn transform(&mut self, field: Option<&str>) {
        if let Some(field) = field.filter(|f| !f.trim().is_empty()) {
            self.standard(field);
        }
    }

    fn policy(&mut self, policy: &PolicyDocument) {
        for block in &policy.blocks {
            match block {
                BlockDoc::Expression { data, .. } => self.standard(data.value.trim()),
                BlockDoc::Assertion { data, .. } => {
                    for condition in &data.conditions {
                        self.standard(condition.expression.trim());
                    }
                }
                BlockDoc::Match { data, .. } => {
                    for arm in &data.arms {
                        self.standard(arm.condition.trim());
                        self.standard(arm.value.trim());
                    }
                }
                BlockDoc::DecisionTable { data, .. } => {
                    for input in &data.inputs {
                        let field = input.field.as_deref().map(str::trim).filter(|f| !f.is_empty());
                        if let Some(field) = field {
                            self.standard(field);
                        }
                        for rule in &data.rules {
                            if let Some(cell) = rule.get(&input.id) {
                                self.cell(field, cell);
                            }
                        }
                    }
                    for output in &data.outputs {
                        for rule in &data.rules {
                            if let Some(cell) = rule.get(&output.id) {
                                self.standard(cell);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn value(&mut self, value: &Value) {
        match value.get("nodes") {
            Some(_) => {
                if let Ok(DecisionContent::Graph(graph)) = serde_json::from_value::<DecisionContent>(value.clone()) {
                    self.graph(&graph);
                }
            }
            None => {
                if let Ok(policy) = serde_json::from_value::<PolicyDocument>(value.clone()) {
                    self.policy(&policy);
                }
            }
        }
    }

    fn workspace(&mut self, workspace: &PolicyWorkspace) {
        for path in workspace.paths() {
            match workspace.is_graph(&path) {
                true => {
                    if let Some(graph) = workspace.get_document(&path).and_then(|d| d.as_graph().cloned()) {
                        self.graph(&graph);
                    }
                }
                false => {
                    if let Some(policy) = workspace.get_policy(&path) {
                        self.policy(&policy);
                    }
                }
            }
        }
    }

    fn sources(&self) -> Vec<(String, ExpressionKind)> {
        let mut out = Vec::new();
        for site in &self.sites {
            match site {
                Site::Standard(source) => out.push((source.clone(), ExpressionKind::Standard)),
                Site::Cell { field, cell } => {
                    out.push((field.clone(), ExpressionKind::Standard));
                    out.push((cell.clone(), ExpressionKind::Unary));
                }
            }
        }
        out
    }
}

struct Corpus;

impl Corpus {
    fn values(values: &[Value]) -> Vec<Variable> {
        values.iter().map(|v| Variable::from(v.clone())).collect()
    }

    fn documents() -> BTreeMap<&'static str, Vec<Document>> {
        let mut corpora: BTreeMap<&'static str, Vec<Document>> = BTreeMap::new();
        for (name, dialect) in [("spec-graph", Dialect::Graph), ("spec-policy", Dialect::Policy)] {
            let (suites, _) = Spec::suites(dialect);
            for suite in suites {
                let inputs = suite.cases.iter().map(|c| c.input.depth_clone(64)).collect();
                let mut document = Document::new(suite.id(), inputs);
                match (dialect, PolicyRun::new(&suite)) {
                    (Dialect::Policy, Ok(run)) => document.workspace(&run.workspace),
                    _ => {
                        document.value(&suite.content);
                        for (_, doc) in &suite.documents {
                            document.value(doc);
                        }
                    }
                }
                corpora.entry(name).or_default().push(document);
            }
        }
        for seed in 0..Generated::seeds() {
            let mut document = Document::new(Generated::label(seed), Self::values(&Generated::inputs(seed)));
            if let Some(workspace) = Generated::workspace(seed) {
                document.workspace(&workspace);
            }
            corpora.entry("generated/policy").or_default().push(document);
        }
        for case in PolicyCase::load_all() {
            let mut document = Document::new(case.name.clone(), Self::values(&case.inputs));
            document.workspace(&case.workspace);
            corpora.entry("policy-fixtures").or_default().push(document);
        }
        for batch in Anchor::batches() {
            let mut document = Document::new(batch.suite.to_string(), Self::values(&batch.values));
            document.workspace(&batch.workspace());
            corpora.entry("anchor").or_default().push(document);
        }
        for fixture in Fixture::load_all() {
            let mut document = Document::new(fixture.name.clone(), Self::values(&fixture.inputs));
            document.graph(&fixture.content);
            corpora.entry("graph-fixtures").or_default().push(document);
        }
        corpora
    }
}

struct Exact;

impl Exact {
    fn same(a: &Variable, b: &Variable) -> bool {
        match (a, b) {
            (Variable::Null, Variable::Null) => true,
            (Variable::Bool(x), Variable::Bool(y)) => x == y,
            (Variable::Number(x), Variable::Number(y)) => x.to_string() == y.to_string(),
            (Variable::String(x), Variable::String(y)) => x == y,
            (Variable::Dynamic(x), Variable::Dynamic(y)) => x.type_name() == y.type_name() && x.to_string() == y.to_string(),
            (Variable::Array(x), Variable::Array(y)) => {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| Self::same(p, q))
            }
            (Variable::Object(x), Variable::Object(y)) => {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| Self::same(p, q)))
            }
            _ => false,
        }
    }

    fn render(result: &Result<Variable, IsolateError>) -> String {
        match result {
            Ok(v) => format!("{v:?}"),
            Err(e) => format!("!{e} {e:?}"),
        }
    }

    fn matches(a: &Result<Variable, IsolateError>, b: &Result<Variable, IsolateError>) -> bool {
        match (a, b) {
            (Ok(x), Ok(y)) => Self::same(x, y),
            (Err(x), Err(y)) => format!("{x} {x:?}") == format!("{y} {y:?}"),
            _ => false,
        }
    }

    fn compile(source: &str, kind: &ExpressionKind) -> (String, String) {
        let mut isolate = Isolate::new();
        let vm = match kind {
            ExpressionKind::Standard => isolate.compile_standard(source).map(|_| ()),
            ExpressionKind::Unary => isolate.compile_unary(source).map(|_| ()),
        };
        let lane = LaneProgram::compile_exact(source, kind.clone()).map(|_| ());
        (format!("{vm:?}"), format!("{lane:?}"))
    }
}

#[derive(Default)]
struct Tally {
    sources: usize,
    compile_errors: usize,
    guarded: usize,
    compile_diffs: Vec<String>,
    runs: usize,
    runtime_errors: usize,
    runtime_diffs: Vec<String>,
    unstable: usize,
}

impl Tally {
    fn compile(&mut self, sources: &BTreeSet<(String, bool)>) {
        for (source, unary) in sources {
            let kind = match unary {
                true => ExpressionKind::Unary,
                false => ExpressionKind::Standard,
            };
            self.sources += 1;
            let (vm, lane) = Exact::compile(source, &kind);
            if vm.starts_with("Err") {
                self.compile_errors += 1;
            }
            if lane.starts_with("Ok") && LaneProgram::compile(source, kind.clone()).is_err() {
                self.guarded += 1;
            }
            if vm != lane {
                self.compile_diffs.push(format!("{kind:?} {source:?} | vm {vm} | lane {lane}"));
            }
        }
    }

    fn pair(&mut self, label: &str, site: &str, want: Result<Variable, IsolateError>, got: Result<Variable, IsolateError>) {
        self.runs += 1;
        if want.is_err() {
            self.runtime_errors += 1;
        }
        if !Exact::matches(&want, &got) {
            self.runtime_diffs
                .push(format!("{label} | {site} | session {} | isolate {}", Exact::render(&got), Exact::render(&want)));
        }
    }

    fn run(&mut self, document: &Document) {
        let programs = Arc::new(Programs::compile(document.sources().iter().map(|(s, k)| (s.as_str(), k.clone()))));
        for input in &document.inputs {
            let mut isolate = Isolate::with_environment(input.deep_clone());
            let mut session = Session::with_environment(input.deep_clone()).with_programs(Some(programs.clone()));
            for site in &document.sites {
                match site {
                    Site::Standard(source) => {
                        let again = Isolate::with_environment(input.deep_clone()).run_standard(source);
                        let want = isolate.run_standard(source);
                        if !Exact::matches(&want, &again) {
                            self.unstable += 1;
                            continue;
                        }
                        let got = session.run_standard(source);
                        self.pair(&document.label, source, want, got);
                    }
                    Site::Cell { field, cell } => {
                        let want = isolate.set_reference(field).map(|_| Variable::Null);
                        let got = session.set_reference(field).map(|_| Variable::Null);
                        let referenced = want.is_ok();
                        self.pair(&document.label, &format!("reference {field}"), want, got);
                        if !referenced {
                            continue;
                        }
                        let want = isolate.run_unary(cell).map(Variable::Bool);
                        let got = session.run_unary(cell).map(Variable::Bool);
                        self.pair(&document.label, &format!("{field} | {cell}"), want, got);
                    }
                }
            }
        }
    }
}

#[test]
#[ignore]
fn lane_census_matches_the_vm() {
    Spec::prepare();
    let corpora = Corpus::documents();
    let mut failures = Vec::new();
    let mut lines = Vec::new();
    for (name, documents) in &corpora {
        let mut tally = Tally::default();
        let mut sources = BTreeSet::new();
        for document in documents {
            for (source, kind) in document.sources() {
                sources.insert((source, kind == ExpressionKind::Unary));
            }
        }
        tally.compile(&sources);
        for document in documents {
            tally.run(document);
        }
        lines.push(format!(
            "{name}: documents {} | sources {} (vm compile errors {}, beyond lane guard {}) | compile diffs {} | runs {} (vm errors {}, nondeterministic skipped {}) | runtime diffs {}",
            documents.len(),
            tally.sources,
            tally.compile_errors,
            tally.guarded,
            tally.compile_diffs.len(),
            tally.runs,
            tally.runtime_errors,
            tally.unstable,
            tally.runtime_diffs.len()
        ));
        failures.extend(tally.compile_diffs.iter().take(20).map(|d| format!("{name} compile: {d}")));
        failures.extend(tally.runtime_diffs.iter().take(20).map(|d| format!("{name} runtime: {d}")));
    }
    eprintln!("{}", lines.join("\n"));
    assert_eq!(Fallbacks::reached(), 0, "lane reached a NumberConversionError fallback");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

mod conformance;

use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::lane::{LaneProgram, Programs, Session};
use zen_expression::variable::Symbol;
use zen_expression::{ExpressionKind, Isolate, IsolateError, Variable};

struct Identity {
    known: HashMap<usize, String>,
}

impl Identity {
    fn of(isolate: &zen_expression::Scope) -> Self {
        let mut identity = Self {
            known: HashMap::new(),
        };
        identity.index(isolate.base(), "env".to_string());
        for (name, value) in isolate.locals() {
            identity.index(value, format!("local:{name}"));
        }
        identity
    }

    fn address(value: &Variable) -> Option<usize> {
        match value {
            Variable::Array(a) => Some(Rc::as_ptr(a) as *const () as usize),
            Variable::Object(o) => Some(Rc::as_ptr(o) as *const () as usize),
            _ => None,
        }
    }

    fn index(&mut self, value: &Variable, path: String) {
        let Some(address) = Self::address(value) else {
            return;
        };
        if self.known.contains_key(&address) {
            return;
        }
        self.known.insert(address, path.clone());
        match value {
            Variable::Array(a) => {
                for (i, item) in a.borrow().iter().enumerate() {
                    self.index(item, format!("{path}[{i}]"));
                }
            }
            Variable::Object(o) => {
                for (k, item) in o.borrow().iter() {
                    self.index(item, format!("{path}.{k}"));
                }
            }
            _ => {}
        }
    }

    fn print(&self, value: &Variable) -> String {
        let mut fresh = HashMap::new();
        let mut out = String::new();
        self.walk(value, &mut fresh, &mut out);
        out
    }

    fn walk(&self, value: &Variable, fresh: &mut HashMap<usize, usize>, out: &mut String) {
        let Some(address) = Self::address(value) else {
            out.push('_');
            return;
        };
        if let Some(path) = self.known.get(&address) {
            out.push('@');
            out.push_str(path);
            return;
        }
        if let Some(n) = fresh.get(&address) {
            out.push_str(&format!("#{n}"));
            return;
        }
        let n = fresh.len();
        fresh.insert(address, n);
        match value {
            Variable::Array(a) => {
                out.push('[');
                for item in a.borrow().iter() {
                    self.walk(item, fresh, out);
                    out.push(',');
                }
                out.push(']');
            }
            Variable::Object(o) => {
                let o = o.borrow();
                let mut keys: Vec<_> = o.keys().map(|k| k.to_string()).collect();
                keys.sort();
                out.push('{');
                for key in keys {
                    out.push_str(&key);
                    out.push(':');
                    if let Some(item) = o.get_str(&key) {
                        self.walk(item, fresh, out);
                    }
                    out.push(',');
                }
                out.push('}');
            }
            _ => {}
        }
    }
}

struct Strict;

impl Strict {
    fn same(a: &Variable, b: &Variable) -> bool {
        match (a, b) {
            (Variable::Number(x), Variable::Number(y)) => x.to_string() == y.to_string(),
            (Variable::Array(x), Variable::Array(y)) => {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| Self::same(p, q))
            }
            (Variable::Object(x), Variable::Object(y)) => {
                let (x, y) = (x.borrow(), y.borrow());
                x.len() == y.len()
                    && x.iter()
                        .all(|(k, p)| y.get(k).is_some_and(|q| Self::same(p, q)))
            }
            (x, y) => conformance::Spec::same(x, y) && conformance::Spec::same(y, x),
        }
    }

    fn render<T: std::fmt::Debug>(result: &Result<T, IsolateError>) -> String {
        match result {
            Ok(value) => format!("{value:?}"),
            Err(e) => format!("!{}", conformance::Spec::describe(e)),
        }
    }
}

#[derive(Default)]
struct Report {
    compared: usize,
    failures: Vec<String>,
    shared: usize,
    unshared: Vec<String>,
}

impl Report {
    fn assert(&self, what: &str) {
        eprintln!(
            "{what}: compared {} | identity checked {} | identity differences {}",
            self.compared,
            self.shared,
            self.unshared.len()
        );
        let shown: Vec<&String> = self.failures.iter().take(40).collect();
        assert!(
            self.failures.is_empty(),
            "{what}: {} of {} comparisons differ:\n{}",
            self.failures.len(),
            self.compared,
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        conformance::Spec::no_fallbacks();
    }

    fn assert_identity(&self, what: &str) {
        let shown: Vec<&String> = self.unshared.iter().take(40).collect();
        assert!(
            self.unshared.is_empty(),
            "{what}: {} Rc identity differences:\n{}",
            self.unshared.len(),
            shown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

struct Pair<'r> {
    isolate: Isolate,
    session: Session,
    report: &'r mut Report,
    label: String,
}

impl<'r> Pair<'r> {
    fn new(report: &'r mut Report, label: &str, programs: Option<Arc<Programs>>) -> Self {
        Self {
            isolate: Isolate::new(),
            session: Session::new().with_programs(programs),
            report,
            label: label.to_string(),
        }
    }

    fn fail(&mut self, step: &str, want: String, got: String) {
        self.report.failures.push(format!(
            "{} | {step} | session {got} vs isolate {want}",
            self.label
        ));
    }

    fn state(&mut self, step: &str) {
        self.report.compared += 1;
        let (a, b) = (self.isolate.scope(), self.session.scope());
        let same_base = Strict::same(a.base(), b.base());
        let same_locals = a.locals().len() == b.locals().len()
            && a.locals()
                .iter()
                .zip(b.locals())
                .all(|((ka, va), (kb, vb))| ka.as_str() == kb.as_str() && Strict::same(va, vb));
        if !same_base || !same_locals {
            let want = format!("{:?} {:?}", a.base(), a.locals());
            let got = format!("{:?} {:?}", b.base(), b.locals());
            self.fail(&format!("{step} state"), want, got);
        }
    }

    fn compare(
        &mut self,
        step: &str,
        want: Result<Variable, IsolateError>,
        got: Result<Variable, IsolateError>,
    ) {
        self.report.compared += 1;
        let same = match (&want, &got) {
            (Ok(a), Ok(b)) => Strict::same(a, b),
            (Err(a), Err(b)) => {
                format!("{a:?}") == format!("{b:?}") && a.to_string() == b.to_string()
            }
            _ => false,
        };
        if !same {
            self.fail(step, Strict::render(&want), Strict::render(&got));
            return;
        }
        if let (Ok(a), Ok(b)) = (&want, &got) {
            self.report.shared += 1;
            let (left, right) = (
                Identity::of(self.isolate.scope()).print(a),
                Identity::of(self.session.scope()).print(b),
            );
            if left != right {
                self.report.unshared.push(format!(
                    "{} | {step} | session {right} vs isolate {left}",
                    self.label
                ));
            }
        }
        self.state(step);
    }

    fn environment(&mut self, env: &Variable) {
        self.isolate.set_environment(env.deep_clone());
        self.session.set_environment(env.deep_clone());
        self.state("set_environment");
    }

    fn local(&mut self, name: &str, value: &Variable) {
        self.isolate
            .set_local(Symbol::from(name), value.deep_clone());
        self.session
            .set_local(Symbol::from(name), value.deep_clone());
        self.state(&format!("set_local {name}"));
    }

    fn standard(&mut self, source: &str) -> Option<Variable> {
        let want = self.isolate.run_standard(source);
        let got = self.session.run_standard(source);
        let result = want.as_ref().ok().cloned();
        self.compare(&format!("run_standard {source}"), want, got);
        result
    }

    fn unary(&mut self, source: &str) -> bool {
        let want = self.isolate.run_unary(source);
        let got = self.session.run_unary(source);
        let result = want.as_ref().is_ok_and(|b| *b);
        self.compare(
            &format!("run_unary {source}"),
            want.map(Variable::Bool),
            got.map(Variable::Bool),
        );
        result
    }

    fn dollar(&mut self, path: &str, value: &Variable) {
        self.isolate.insert_dollar(path, value.deep_clone());
        self.session.insert_dollar(path, value.deep_clone());
        self.state(&format!("insert_dollar {path}"));
    }

    fn produced_dollar(&mut self, path: &str, source: &str) {
        let want = self.isolate.run_standard(source);
        let got = self.session.run_standard(source);
        let (a, b) = match (&want, &got) {
            (Ok(a), Ok(b)) => (a.clone(), b.clone()),
            _ => (Variable::Null, Variable::Null),
        };
        self.compare(&format!("run_standard {source}"), want, got);
        let (a, b) = match source.contains('$') {
            true => (a.deep_clone(), b.deep_clone()),
            false => (a, b),
        };
        self.isolate.insert_dollar(path, a);
        self.session.insert_dollar(path, b);
        self.state(&format!("insert_dollar {path}"));
    }

    fn reference(&mut self, field: &str) -> bool {
        let want = self.isolate.set_reference(field);
        let got = self.session.set_reference(field);
        let ok = want.is_ok();
        self.compare(
            &format!("set_reference {field}"),
            want.map(|_| Variable::Null),
            got.map(|_| Variable::Null),
        );
        self.get_reference(field);
        ok
    }

    fn get_reference(&mut self, field: &str) {
        let want = self.isolate.get_reference(field);
        let got = self.session.get_reference(field);
        self.report.compared += 1;
        let same = match (&want, &got) {
            (Some(a), Some(b)) => Strict::same(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.fail(
                &format!("get_reference {field}"),
                format!("{want:?}"),
                format!("{got:?}"),
            );
        }
    }

    fn reference_value(&mut self, value: &Variable) {
        let want = self.isolate.set_reference_value(value.deep_clone());
        let got = self.session.set_reference_value(value.deep_clone());
        self.compare(
            "set_reference_value",
            want.map(|_| Variable::Null),
            got.map(|_| Variable::Null),
        );
    }

    fn reference_from(&mut self, field: &str) -> bool {
        let want = self.isolate.run_standard(field);
        let got = self.session.run_standard(field);
        let values = match (&want, &got) {
            (Ok(a), Ok(b)) => Some((a.clone(), b.clone())),
            _ => None,
        };
        self.compare(&format!("run_standard {field}"), want, got);
        let Some((a, b)) = values else {
            return false;
        };
        let _ = self.isolate.set_reference_value(a);
        let _ = self.session.set_reference_value(b);
        self.state("set_reference_value");
        true
    }
}

struct Scenarios;

impl Scenarios {
    fn env() -> Variable {
        Variable::from(json!({
            "customer": {"age": 34, "name": "Ann", "tier": "gold", "tags": ["a", "b"], "score": 1.50, "address": {"city": "Split"}},
            "order": {"total": 120.25, "items": [{"sku": "x", "qty": 2, "price": 10.5}, {"sku": "y", "qty": 1, "price": 99.25}]},
            "flags": [true, false],
            "matrix": [[1, 2], [3, 4]],
            "empty": {},
            "none": null,
            "text": "hello",
            "when": "2024-03-15T10:00:00Z",
            "$": {"stale": true}
        }))
    }

    fn nodes() -> Variable {
        Variable::from(json!({"input": {"a": 1, "nested": {"b": [1, 2]}}, "calc": {"total": 10}}))
    }

    const EXPRESSIONS: &'static [(&'static str, &'static str)] = &[
        ("age", "customer.age"),
        ("adult", "$.age >= 18"),
        ("customer", "customer"),
        ("address", "customer.address"),
        ("copy", "$.customer"),
        ("whole", "$"),
        ("nested.value", "$.address.city + '!'"),
        ("nested.other", "$.nested"),
        ("items", "order.items"),
        ("first", "order.items[0]"),
        ("mapped", "map(order.items, #.qty * #.price)"),
        ("filtered", "filter(order.items, #.qty > 1)"),
        ("node", "$nodes.input.nested"),
        ("nodeArr", "$nodes.input.nested.b"),
        ("obj", "{ c: customer, n: 1, a: customer.address }"),
        ("arr", "[customer, customer.address, order.items[1]]"),
        ("cond", "customer.age > 30 ? customer.address : order"),
        ("coalesce", "none ?? customer"),
        ("missing", "customer.unknown.deeper"),
        ("bad", "customer.age + 'x'"),
        ("after", "$.bad"),
        ("assigned", "x = customer; y = x.address; y"),
        ("date", "d(when)"),
        ("dateBad", "d('not a date')"),
        ("text", "upper(text) + string($.age)"),
        ("keysOf", "keys(customer)"),
        ("slice", "customer.tags[0:1]"),
        ("matrix", "matrix[1]"),
        ("flat", "flatten(matrix)"),
        ("sum", "sum(map(order.items, #.price))"),
        ("call", "abs()"),
        ("unknown", "nofn(1)"),
        ("lexer", "'unterminated"),
        ("parser", "customer.age +"),
        ("closureDollar", "map(order.items, $.age + #.qty)"),
        ("dollarSelf", "$.dollarSelf"),
    ];

    const FIELDS: &'static [&'static str] = &[
        "customer.age",
        "customer",
        "customer.tags",
        "order.items",
        "order.items[0]",
        "none",
        "text",
        "customer.age + 'x'",
        "customer.age +",
        "$",
        "$.stale",
        "$nodes.calc.total",
        "when",
    ];

    const CELLS: &'static [&'static str] = &[
        "> 18",
        "< 18",
        "[18..40]",
        "'Ann'",
        "'gold', 'silver'",
        "contains($, 'a')",
        "$.address.city == 'Split'",
        "len($) > 1",
        "$ == null",
        "$",
        "$ + 1",
        "",
        "'x' + 1",
        "> 'a'",
        "== customer.age",
        "startsWith($, 'h')",
        "d($) > d('2020-01-01')",
        "some($, # == 'a')",
        "$ in [1, 2]",
        "> ",
        "abs()",
    ];

    const STANDALONE: &'static [&'static str] = &[
        "customer.age > 18",
        "customer.age",
        "text",
        "$ == null",
        "none",
        "1 +",
        "customer.age > 'x'",
    ];

    fn expression_node(pair: &mut Pair) {
        pair.environment(&Self::env());
        pair.local("$nodes", &Self::nodes());
        for (key, source) in Self::EXPRESSIONS {
            pair.produced_dollar(key, source);
        }
        pair.standard("$");
        pair.standard("$.nested");
        pair.standard("$nodes");
    }

    fn graph_table(pair: &mut Pair) {
        pair.environment(&Self::env());
        pair.local("$nodes", &Self::nodes());
        for field in Self::FIELDS {
            for cell in Self::CELLS {
                if pair.reference(field) {
                    pair.unary(cell);
                }
            }
            pair.get_reference(field);
        }
        for cell in Self::STANDALONE {
            pair.standard(cell);
        }
        for field in Self::FIELDS {
            pair.get_reference(field);
            pair.standard(field);
        }
    }

    fn policy_table(pair: &mut Pair) {
        pair.environment(&Self::env());
        for field in Self::FIELDS {
            if pair.reference_from(field) {
                for cell in Self::CELLS {
                    pair.unary(cell);
                }
            }
        }
        pair.reference_value(&Variable::from(json!({"k": [1, 2, 3]})));
        pair.unary("len($.k) == 3");
        pair.standard("$");
    }

    fn switching(pair: &mut Pair) {
        pair.environment(&Self::env());
        pair.reference("customer.age");
        pair.dollar("x.y", &Variable::from(json!({"z": 1})));
        pair.standard("$");
        pair.environment(&Variable::from(json!({"customer": {"age": 7}})));
        pair.get_reference("customer.age");
        pair.standard("$");
        pair.unary("> 5");
        pair.reference("customer.age");
        pair.unary("> 5");
        pair.environment(&Variable::Null);
        pair.standard("customer.age");
        pair.standard("$");
        pair.unary("$ == null");
        pair.dollar("a", &Variable::from(json!(1)));
        pair.standard("$.a");
        pair.environment(&Variable::from(json!([1, 2, 3])));
        pair.standard("$");
        pair.standard("len($root)");
        pair.environment(&Variable::from(json!("text")));
        pair.standard("upper($root)");
        pair.reference_value(&Variable::from(json!(5)));
        pair.dollar("a.b", &Variable::from(json!(2)));
        pair.standard("$");
        pair.reference_value(&Variable::from(json!({"q": 1})));
        pair.dollar("w", &Variable::from(json!(2)));
        pair.standard("$");
        pair.local("$", &Variable::from(json!("local")));
        pair.standard("$");
        pair.unary("'local'");
        pair.unary("1 + 1");
        pair.unary("'x'");
    }

    fn policy_driver(pair: &mut Pair) {
        let env = Self::env();
        for round in 0..3 {
            pair.environment(&env);
            pair.standard("customer.age >= 18 and customer.tier != 'none'");
            pair.standard("order.total * 2");
            pair.standard("customer.age / 0");
            pair.standard("customer.name > 3");
            pair.standard("sum(map(order.items, #.qty))");
            pair.standard(match round {
                0 => "$",
                1 => "$.stale",
                _ => "$root",
            });
            pair.reference_from("customer.age");
            pair.unary("> 18");
            pair.standard("$");
        }
    }

    fn all(report: &mut Report, programs: Option<Arc<Programs>>, label: &str) {
        for name in [
            "expression node",
            "graph table",
            "policy table",
            "switching",
            "policy driver",
        ] {
            for _ in 0..2 {
                let mut pair = Pair::new(report, &format!("{label}: {name}"), programs.clone());
                for _ in 0..2 {
                    match name {
                        "expression node" => Self::expression_node(&mut pair),
                        "graph table" => Self::graph_table(&mut pair),
                        "policy table" => Self::policy_table(&mut pair),
                        "switching" => Self::switching(&mut pair),
                        _ => Self::policy_driver(&mut pair),
                    }
                }
            }
        }
    }

    fn sources() -> Vec<(&'static str, ExpressionKind)> {
        let mut sources = Vec::new();
        for (_, source) in Self::EXPRESSIONS {
            sources.push((*source, ExpressionKind::Standard));
        }
        for field in Self::FIELDS.iter().chain(Self::STANDALONE) {
            sources.push((*field, ExpressionKind::Standard));
        }
        for cell in Self::CELLS {
            sources.push((*cell, ExpressionKind::Unary));
        }
        sources
    }
}

struct Checks;

impl Checks {
    fn sendable<T: Send + Sync>() {}
}

#[test]
fn session_matches_isolate_in_executor_sequences() {
    conformance::Spec::prepare();
    let mut report = Report::default();
    Scenarios::all(&mut report, None, "fallback");
    let programs = Arc::new(Programs::compile(Scenarios::sources()));
    Scenarios::all(&mut report, Some(programs.clone()), "precompiled");
    Scenarios::all(&mut report, Some(Arc::new(Programs::new())), "empty");
    report.assert("session scenarios");
    report.assert_identity("session scenarios");
}

#[test]
fn session_programs_are_shared_across_threads() {
    Checks::sendable::<Programs>();
    Checks::sendable::<Arc<Programs>>();
    let programs = Arc::new(Programs::compile([
        ("a + 1", ExpressionKind::Standard),
        ("> 1", ExpressionKind::Unary),
    ]));
    let handles: Vec<_> = (0..3)
        .map(|i| {
            let programs = programs.clone();
            std::thread::spawn(move || {
                let mut session = Session::with_environment(Variable::from(json!({"a": i})))
                    .with_programs(Some(programs));
                let value = session.run_standard("a + 1").map(|v| v.to_value());
                let _ = session.set_reference_value(Variable::from(json!(i)));
                let unary = session.run_unary("> 1");
                (value.ok(), unary.ok())
            })
        })
        .collect();
    let got: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    assert_eq!(
        got,
        vec![
            (Some(json!(1)), Some(false)),
            (Some(json!(2)), Some(false)),
            (Some(json!(3)), Some(true))
        ]
    );
    assert_eq!(programs.len(), 2);
}

#[test]
fn session_compile_errors_match_isolate_exactly() {
    let sources: &[(&str, ExpressionKind)] = &[
        ("'abc", ExpressionKind::Standard),
        ("a +", ExpressionKind::Standard),
        ("abs()", ExpressionKind::Standard),
        ("unknownFn(1)", ExpressionKind::Standard),
        ("a.b(", ExpressionKind::Standard),
        ("x = ; 1", ExpressionKind::Standard),
        ("[1, 2", ExpressionKind::Standard),
        ("> ", ExpressionKind::Unary),
        ("'a', ", ExpressionKind::Unary),
        ("abs()", ExpressionKind::Unary),
        ("$.a.method()", ExpressionKind::Standard),
        ("len(1, 2)", ExpressionKind::Standard),
        ("`${a`", ExpressionKind::Standard),
        ("#", ExpressionKind::Standard),
        ("1 ?? ", ExpressionKind::Standard),
    ];
    let mut failures = Vec::new();
    for (source, kind) in sources {
        let mut isolate = Isolate::new();
        let want = match kind {
            ExpressionKind::Standard => isolate.compile_standard(source).map(|_| ()),
            ExpressionKind::Unary => isolate.compile_unary(source).map(|_| ()),
        };
        let got = LaneProgram::compile(source, kind.clone()).map(|_| ());
        let (want, got) = (format!("{want:?}"), format!("{got:?}"));
        if want != got {
            failures.push(format!("{source} | lane {got} vs vm {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    conformance::Spec::no_fallbacks();
}

#[test]
fn session_matches_isolate_over_the_spec() {
    conformance::Spec::prepare();
    let (cases, problems) = conformance::Spec::cases();
    assert!(
        problems.is_empty(),
        "malformed spec:\n{}",
        problems.join("\n")
    );
    let mut report = Report::default();
    let mut groups: BTreeMap<(bool, String), Vec<&conformance::SpecCase>> = BTreeMap::new();
    for case in &cases {
        let unary = matches!(case.kind, ExpressionKind::Unary);
        groups
            .entry((unary, case.expression.clone()))
            .or_default()
            .push(case);
    }
    let sources: Vec<(&str, ExpressionKind)> = groups
        .keys()
        .map(|(unary, source)| {
            let kind = match unary {
                true => ExpressionKind::Unary,
                false => ExpressionKind::Standard,
            };
            (source.as_str(), kind)
        })
        .collect();
    let programs = Arc::new(Programs::compile(sources));
    let mut skipped = 0usize;
    for ((unary, source), group) in &groups {
        let unstable = group.iter().any(|c| {
            let (a, b) = (c.exact(), c.exact());
            match (&a, &b) {
                (Ok(x), Ok(y)) => !Strict::same(x, y),
                (Err(x), Err(y)) => x != y || x.starts_with(conformance::Spec::PANIC),
                _ => true,
            }
        });
        if unstable {
            skipped += group.len();
            continue;
        }
        let mut pair = Pair::new(&mut report, source, Some(programs.clone()));
        for case in group {
            pair.environment(&case.environment());
            match unary {
                true => {
                    pair.unary(source);
                    if let Some(input) = &case.input {
                        pair.reference_value(input);
                        pair.unary(source);
                    }
                }
                false => {
                    pair.standard(source);
                    pair.dollar("prev", &Variable::from(json!({"v": 1})));
                    pair.standard(source);
                }
            }
        }
    }
    eprintln!(
        "spec groups {} | skipped {skipped} nondeterministic cases",
        groups.len()
    );
    report.assert("session spec");
    let shown: Vec<&String> = report.unshared.iter().take(60).collect();
    eprintln!(
        "identity differences {}:\n{}",
        report.unshared.len(),
        shown
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    report.assert_identity("session spec");
}

#[test]
fn session_shares_rc_like_isolate() {
    let env = Variable::from(json!({
        "customer": {"address": {"city": "Split"}, "tags": ["a"]},
        "items": [{"a": 1}, {"a": 2}]
    }));
    let nodes = Variable::from(json!({"input": {"x": {"y": 1}}}));
    let cases: &[(&str, &str)] = &[
        ("member object", "customer"),
        ("nested member", "customer.address"),
        ("member array", "customer.tags"),
        ("index", "items[0]"),
        ("dollar passthrough", "$"),
        ("dollar member", "$.address"),
        ("nodes member", "$nodes.input.x"),
        ("inserted dollar", "$.inserted"),
        ("conditional", "true ? customer : null"),
        ("coalesce", "null ?? customer"),
        ("object literal", "{ c: customer }"),
        ("array literal", "[customer]"),
        ("root", "$root"),
        ("filter", "filter(items, true)"),
        ("map identity", "map(items, #)"),
        ("slice", "items[0:1]"),
    ];
    let mut rows = Vec::new();
    let mut differences = Vec::new();
    for (name, source) in cases {
        let mut isolate = Isolate::with_environment(env.deep_clone());
        let mut session = Session::with_environment(env.deep_clone());
        isolate.set_local(Variable::nodes_key(), nodes.deep_clone());
        session.set_local(Variable::nodes_key(), nodes.deep_clone());
        isolate
            .set_reference("customer")
            .expect("isolate reference");
        session
            .set_reference("customer")
            .expect("session reference");
        let inserted = Variable::from(json!({"k": [1]}));
        isolate.insert_dollar("inserted", inserted.deep_clone());
        session.insert_dollar("inserted", inserted.deep_clone());
        let (Ok(want), Ok(got)) = (isolate.run_standard(source), session.run_standard(source))
        else {
            differences.push(format!("{name}: {source} failed"));
            continue;
        };
        let left = Identity::of(isolate.scope()).print(&want);
        let right = Identity::of(session.scope()).print(&got);
        rows.push(format!(
            "{name:<20} {source:<24} isolate {left} | session {right}"
        ));
        if left != right {
            differences.push(format!("{name}: {source} isolate {left} session {right}"));
        }
        let reference = (
            isolate.get_reference("customer"),
            session.get_reference("customer"),
        );
        if let (Some(Variable::Object(a)), Some(Variable::Object(b))) = reference {
            let base_a = isolate.scope().base().dot("customer");
            let base_b = session.scope().base().dot("customer");
            let shared_a = matches!(base_a, Some(Variable::Object(o)) if Rc::ptr_eq(&o, &a));
            let shared_b = matches!(base_b, Some(Variable::Object(o)) if Rc::ptr_eq(&o, &b));
            if shared_a != shared_b {
                differences.push(format!(
                    "{name}: reference sharing isolate {shared_a} session {shared_b}"
                ));
            }
        }
    }
    eprintln!("{}", rows.join("\n"));
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

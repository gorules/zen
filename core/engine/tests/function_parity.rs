use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EngineVersion, EvaluationOptions, Variable};

const SNIPPETS: &[&str] = &[
    "typeof Intl",
    "typeof globalThis.Intl",
    "'Intl' in globalThis",
    "Object.keys(globalThis)",
    "JSON.stringify(Object.getOwnPropertyNames(globalThis))",
    "(() => { const names = []; for (const name in globalThis) names.push(name); return names; })()",
    "Object.getOwnPropertyNames(globalThis).filter((name) => name.startsWith('__'))",
    "Object.getOwnPropertySymbols(globalThis).length",
    "Date.name",
    "Date.length",
    "Date.prototype.constructor === Date",
    "Object.prototype.toString.call(new Date())",
    "new Date() instanceof Date",
    "new Date(0) instanceof Date",
    "String(Date)",
    "Function.prototype.toString.call(Date)",
    "Object.getOwnPropertyNames(Date)",
    "Object.keys(Date)",
    "Object.keys(Date.prototype)",
    "[Date.now.name, Date.now.length, Date.parse.name, Date.parse.length, Date.UTC.name, Date.UTC.length]",
    "[String(Date.now), String(Date.parse), String(Date.UTC)]",
    "[Math.random.name, Math.random.length, String(Math.random), Object.keys(Math)]",
    "Object.getOwnPropertyNames(Math)",
    "(() => { const d = Object.getOwnPropertyDescriptor(Date, 'prototype'); return [d.writable, d.enumerable, d.configurable]; })()",
    "(() => { const d = Object.getOwnPropertyDescriptor(globalThis, 'Date'); return [d.writable, d.enumerable, d.configurable, 'value' in d]; })()",
    "(() => { const d = Object.getOwnPropertyDescriptor(Math, 'random'); return [d.writable, d.enumerable, d.configurable]; })()",
    "(() => { const d = Object.getOwnPropertyDescriptor(Date, 'now'); return [d.writable, d.enumerable, d.configurable]; })()",
    "(() => { const d = Object.getOwnPropertyDescriptor(Date.prototype, 'getHours'); return [d.writable, d.enumerable, d.configurable]; })()",
    "Object.getOwnPropertyNames(Date.prototype)",
    "Object.getOwnPropertyNames(Date.prototype).filter((n) => typeof Date.prototype[n] === 'function').map((n) => [n, Date.prototype[n].name, Date.prototype[n].length, String(Date.prototype[n]), 'prototype' in Date.prototype[n]])",
    "(() => { new Date(0); return Object.getOwnPropertyNames(Date.prototype).filter((n) => typeof Date.prototype[n] === 'function').map((n) => [n, Date.prototype[n].name, Date.prototype[n].length, String(Date.prototype[n]), 'prototype' in Date.prototype[n]]); })()",
    "(() => { Date.parse('2020'); const d = Object.getOwnPropertyDescriptor(Date.prototype, 'setHours'); return [d.writable, d.enumerable, d.configurable, Object.getOwnPropertyNames(d.value)]; })()",
    "(() => { new Date(0); return [Object.keys(Date.prototype), Object.getOwnPropertyNames(Date), Object.keys(Date)]; })()",
    "(() => { new Date(0); try { new (Date.prototype.getHours)(); return 'constructed'; } catch (e) { return e.constructor.name; } })()",
    "(() => { new Date(0); try { return Date.prototype.getHours.call({}); } catch (e) { return e.constructor.name; } })()",
    "(() => { try { new (Date.prototype.getHours)(); return 'constructed'; } catch (e) { return e.constructor.name; } })()",
    "(() => { try { new Date.now(); return 'constructed'; } catch (e) { return e.constructor.name; } })()",
    "(() => { try { return Date.prototype.getHours.call({}); } catch (e) { return e.constructor.name; } })()",
    "Object.getOwnPropertyNames(Date.prototype.getHours)",
    "Object.getOwnPropertyNames(Date.now)",
    "typeof Date()",
    "Date().length > 20",
    "Date(2020, 1) === Date()",
    "(() => { class Later extends Date {} const d = new Later(0); return [d instanceof Later, d instanceof Date, d.getTime(), Object.prototype.toString.call(d)]; })()",
    "new Date(2020, 0, 1).getTime() === new Date(2020, 0, 1, 0, 0, 0, 0).getTime()",
    "new Date(2020, 5, 15, 10, 30).toString()",
    "new Date(2020, 5, 15, 10, 30).toLocaleString()",
    "new Date(1700000000000).toLocaleDateString()",
    "new Date(1700000000000).toLocaleTimeString()",
    "typeof new Date(0).toLocaleString('en-US', { timeZone: 'utc' })",
    "typeof new Date(0).toLocaleString('en-US', { timeZone: 'UTC' })",
    "typeof new Date(0).toLocaleString('en-US', { timeZone: 'Utc' })",
    "typeof new Date(0).toLocaleDateString('en-US', { timeZone: 'europe/berlin' })",
    "typeof new Date(0).toLocaleTimeString('en-US', { timeZone: 'AMERICA/NEW_YORK' })",
    "typeof new Date(0).toLocaleString(undefined, { timeZone: 'Mars/Base' })",
    "typeof new Date(0).toLocaleString(undefined, { timeZone: 42 })",
    "new Date('2020-02-03T04:05:06').getHours()",
    "Date.parse('2020-02-03T04:05:06Z')",
    "new Date(NaN).toString()",
    "new Date(8.64e15 + 1).getTime()",
    "[new Date(0).setHours(), new Date(NaN).setFullYear(2020) > 0, new Date(NaN).setMonth(1)]",
    "typeof Math.random()",
    "Math.random() >= 0 && Math.random() < 1",
    "typeof Date.now()",
    "[typeof Date.call(Date), typeof Date.call(Array, 2020, 1), typeof Reflect.apply(Date, Array, [])]",
    "Object.getPrototypeOf(Date) === Function.prototype",
    "(() => { new Date(0); return [Object.getPrototypeOf(Date.prototype.getHours) === Function.prototype, Object.getPrototypeOf(Date.now) === Function.prototype, Object.getPrototypeOf(Math.random) === Function.prototype, Date.prototype.getHours instanceof Function]; })()",
    "[Date.prototype.constructor === Date, globalThis.Date === Date, Date.prototype instanceof Date]",
    "(() => { const d = Object.getOwnPropertyDescriptor(Date, 'length'); const n = Object.getOwnPropertyDescriptor(Date, 'name'); return [d.value, d.writable, d.enumerable, d.configurable, n.value, n.writable, n.enumerable, n.configurable]; })()",
    "(() => { class A extends Date { constructor() { super(2020, 1, 3); } } const a = new A(); return [a instanceof A, a instanceof Date, a.getMonth(), a.getDate(), Object.getPrototypeOf(a) === A.prototype]; })()",
    "(() => { const B = Date.bind(null, 2020, 1); const b = new B(); return [b instanceof Date, b.getMonth(), typeof B()]; })()",
    "Reflect.construct(Date, [2020, 1, 3], Object).constructor === Object",
    "[new Date(new Date(5)).getTime(), new Date(2020, 1, 3).getDate(), new Date(0).setTime(7)]",
    "[Function.prototype.toString.call(Date.now), String(Date.prototype.getHours), String(Date.prototype.setFullYear), Date.prototype.toLocaleString.length]",
    "(() => { try { Date.prototype.getHours.call(Object.create(Date.prototype)); return 'ok'; } catch (e) { return [e.constructor.name, e.message]; } })()",
    "(() => { try { Date.prototype.setHours.call(1, 2); return 'ok'; } catch (e) { return [e.constructor.name, e.message]; } })()",
    "(() => { try { new Date(Symbol()); return 'ok'; } catch (e) { return e.constructor.name; } })()",
    "(() => { const seen = []; new Date(0).setHours({ valueOf() { seen.push('h'); return 1; } }, { valueOf() { seen.push('m'); return 2; } }); return seen; })()",
    "new Date(2020, 0, 1, 12).getDay() + new Date(2020, 0, 1, 12).getYear()",
];

#[derive(Clone, Copy, Debug)]
enum Path {
    Repeated,
    Compiled,
    Batch,
}

fn graph(content: Value) -> Value {
    json!({
        "nodes": [
            {"id": "in", "name": "in", "type": "inputNode"},
            {"id": "node", "name": "node", "type": "functionNode", "content": content},
            {"id": "out", "name": "out", "type": "outputNode"}
        ],
        "edges": [
            {"id": "e1", "sourceId": "in", "targetId": "node", "type": "edge"},
            {"id": "e2", "sourceId": "node", "targetId": "out", "type": "edge"}
        ]
    })
}

fn legacy(body: &str) -> Value {
    graph(json!(format!(
        "const handler = (input) => ({{ v: {body} }})"
    )))
}

fn modern(body: &str) -> Value {
    graph(json!({"source": format!("export const handler = async (input) => ({{ v: {body} }});")}))
}

fn shown(result: Result<Value, String>) -> Value {
    result.unwrap_or_else(|error| json!({ "error": error }))
}

fn outcomes(version: EngineVersion, path: Path, graph: &Value) -> Vec<Value> {
    let content: DecisionContent = serde_json::from_value(graph.clone()).unwrap();
    let engine = DecisionEngine::default().with_version(version);
    let mut decision = engine.create_decision(Arc::new(content)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let one = |decision: &zen_engine::Decision| {
        shown(
            runtime
                .block_on(decision.evaluate_with_opts(
                    Variable::from(json!({"x": 1})),
                    EvaluationOptions::default(),
                ))
                .map(|response| response.result.to_value())
                .map_err(|error| error.to_string()),
        )
    };
    match path {
        Path::Repeated => (0..3).map(|_| one(&decision)).collect(),
        Path::Compiled => {
            decision.compile();
            (0..2).map(|_| one(&decision)).collect()
        }
        Path::Batch => {
            let rows = vec![Variable::from(json!({"x": 1})); 3];
            runtime
                .block_on(decision.evaluate_batch(&rows, EvaluationOptions::default()))
                .into_iter()
                .map(|row| {
                    shown(
                        row.map(|response| response.result.to_value())
                            .map_err(|error| error.to_string()),
                    )
                })
                .collect()
        }
    }
}

fn differences(build: fn(&str) -> Value) -> Vec<String> {
    let mut found = Vec::new();
    for body in SNIPPETS {
        let graph = build(body);
        let want = outcomes(EngineVersion::V1, Path::Repeated, &graph).remove(0);
        for path in [Path::Repeated, Path::Compiled, Path::Batch] {
            for (call, got) in outcomes(EngineVersion::V2, path, &graph)
                .into_iter()
                .enumerate()
            {
                if got != want {
                    found.push(format!(
                        "{body}\n  {path:?} call {call}\n  v1: {want}\n  v2: {got}"
                    ));
                }
            }
        }
    }
    found
}

#[test]
fn legacy_function_nodes_see_the_same_globals_and_dates_on_both_versions() {
    let found = differences(legacy);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn function_nodes_see_the_same_globals_and_dates_on_both_versions() {
    let found = differences(modern);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

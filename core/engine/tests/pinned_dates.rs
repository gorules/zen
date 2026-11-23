use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use zen_engine::context::{parse_instant, parse_timezone};
use zen_engine::model::DecisionContent;
use zen_engine::{DecisionEngine, EngineVersion, EvaluationOptions, Variable};

const ZONES: &[&str] = &[
    "UTC",
    "Europe/Berlin",
    "America/St_Johns",
    "Asia/Kathmandu",
    "Pacific/Kiritimati",
    "Etc/GMT+5",
    "Australia/Lord_Howe",
];

const FIXTURE: &str = "tests/fixtures/pinned_dates.json";

const SCRIPT: &str = r#"
const show = (v) => {
  if (typeof v === 'number') return Object.is(v, -0) ? '-0' : String(v);
  if (typeof v === 'bigint') return v + 'n';
  return v;
};
const attempt = (f) => {
  try { return show(f()); } catch (e) { return 'throws ' + e.constructor.name + ': ' + e.message; }
};
const bases = [
  0, -1, 1, 1700000000000, 1768480496789,
  Date.UTC(2026, 2, 29, 1), Date.UTC(2026, 9, 25, 1),
  Date.UTC(2026, 2, 8, 5, 30), Date.UTC(2026, 10, 1, 4, 30),
  Date.UTC(1994, 11, 31, 10), Date.UTC(1985, 11, 31, 18, 15),
  Date.UTC(2026, 3, 4, 15), Date.UTC(2026, 9, 3, 15, 30),
  Date.UTC(1893, 3, 1), Date.UTC(1800, 0, 1, 12), Date.UTC(1900, 0, 1),
  Date.UTC(2099, 11, 31, 23), Date.UTC(2100, 2, 28, 1), Date.UTC(2100, 6, 1, 12), Date.UTC(2250, 9, 31),
  Date.UTC(2399, 11, 31, 23, 59), Date.UTC(9999, 11, 31, 23, 59, 59, 999),
  Date.UTC(0, 0, 1) - 62167219200000 * 0, Date.UTC(-1, 5, 15), Date.UTC(-271821, 3, 20),
  8.64e15, -8.64e15, 8.64e15 - 3600000, -8.64e15 + 86400000, 8.6e15, -8.6e15, NaN,
];
const instants = [];
for (const b of bases) {
  if (Number.isNaN(b)) { instants.push(b); continue; }
  for (const k of [-3, -2, -1, 0, 1, 2, 3]) {
    const t = b + k * 1800000 + (k === 1 ? 123 : 0);
    if (Math.abs(t) <= 8.64e15) instants.push(t);
  }
}
const getters = ['getFullYear', 'getMonth', 'getDate', 'getDay', 'getHours', 'getMinutes', 'getSeconds', 'getMilliseconds', 'getTimezoneOffset', 'getYear'];
const strings = ['toString', 'toDateString', 'toTimeString', 'toLocaleString', 'toLocaleDateString', 'toLocaleTimeString'];
const setters = [
  ['setHours', [2]], ['setHours', [2, 30]], ['setHours', [3, 15, 10, 5]], ['setHours', [25]], ['setHours', [-1]],
  ['setHours', [1.7]], ['setHours', [-1.7]], ['setHours', []], ['setHours', [NaN]], ['setHours', ['3']], ['setHours', [Infinity]],
  ['setMinutes', [90]], ['setMinutes', [-30, 15]], ['setSeconds', [3601, 5]], ['setMilliseconds', [1e9]],
  ['setDate', [0]], ['setDate', [32]], ['setMonth', [13]], ['setMonth', [-1, 31]], ['setMonth', [1, 29]],
  ['setFullYear', [2100]], ['setFullYear', [-1, 1, 29]], ['setFullYear', [1e6]], ['setFullYear', [275760, 8, 13]], ['setFullYear', [2026, 2, 29]],
  ['setFullYear', [99]], ['setFullYear', [null, undefined]], ['setDate', [{ valueOf: () => 15 }]], ['setHours', [2n]],
];
const rows = instants.map((t) => {
  const d = new Date(t);
  const row = { t: show(t) };
  for (const g of getters) row[g] = attempt(() => d[g]());
  for (const s of strings) row[s] = attempt(() => d[s]());
  row.localeZoned = attempt(() => d.toLocaleString(undefined, { timeZone: 'Asia/Kathmandu' }));
  row.localeLower = attempt(() => d.toLocaleTimeString('en', { timeZone: 'america/st_johns' }));
  row.localeUnknown = attempt(() => d.toLocaleDateString('en', { timeZone: 'Mars/Base' }));
  row.localeNumber = attempt(() => d.toLocaleString('en', { timeZone: 42 }));
  row.localeNull = attempt(() => d.toLocaleString('en', null));
  row.sets = setters.map(([name, args]) => {
    const x = new Date(t);
    const r = attempt(() => x[name](...args));
    return r + ' ' + show(x.getTime());
  });
  return row;
});
const constructed = [
  [2026, 2, 29, 2, 30], [2026, 2, 29, 1, 59, 59, 999], [2026, 2, 29, 3], [2026, 9, 25, 2, 30], [2026, 9, 25, 1, 30], [2026, 9, 25, 3],
  [2026, 2, 8, 2, 30], [2026, 10, 1, 1, 30], [2026, 10, 1, 0, 30], [2026, 3, 5, 1, 45], [2026, 3, 5, 2, 15], [2026, 9, 4, 2, 15],
  [1994, 11, 31, 12], [1994, 11, 30, 23, 59], [1995, 0, 1], [1986, 0, 1, 0, 10], [1985, 11, 31, 23, 59],
  [99, 0], [0, 0], [-1, 0], [100, 0], [99.9, 0], [-0.5, 0], [275760, 8, 13], [275760, 8, 12, 23], [-271821, 3, 20], [-271821, 3, 19, 23],
  [2100, 6, 1], [2100, 2, 28, 2, 30], [2250, 9, 31, 2, 30], [2399, 11, 31, 23, 59], [2026, 0, 1, 0, 0, 0, 0.5], [2026, 0, 1, 0, 0, 0, -0.5],
  [NaN, 0], [2026, NaN], [2026, 0, 1, 0, 0, 0, 0, 99], ['2026', '1'], [2026, 1, 1e10], [2026, 1e10], [1e20, 0], [2026, 0, 1, 1e20],
  [Infinity, 0], [2026, -Infinity], [null, null], [undefined, 1], [true, false], [2026n, 1], [2026, 0, { valueOf: () => 5 }],
];
const made = constructed.map((args) => attempt(() => {
  const d = new Date(...args);
  return show(d.getTime()) + ' ' + d.toString();
}));
const order = (() => {
  const log = [];
  const v = (x, n) => ({ valueOf() { log.push(n); return x; } });
  const a = new Date(v(2026, 'y'), v(0, 'm'), v(NaN, 'd'), v(1, 'h'), v(2, 'mi'), v(3, 's'), v(4, 'ms'), v(5, 'extra')).getTime();
  const b = new Date(NaN);
  const c = b.setHours(v(1, 'nan-hours'));
  const e = new Date(0).setMinutes(v(1, 'mi2'), v(2, 's2'), v(3, 'ms2'), v(4, 'extra2'));
  return [show(a), show(c), show(e), log];
})();
const texts = [
  '2026-01-15T10:00:00', '2026-01-15T10:00', '2026-01-15 10:00', '2026-01-15', '2026-01', '2026', '+002026-01-15', '-000001-06-15',
  '2026-01-15T10:00:00Z', '2026-01-15T10:00:00z', '2026-01-15T10:00:00+05:45', '2026-01-15T10:00:00-0330', '2026-10-25T02:30:00',
  'Thu Jan 15 2026 10:00:00 GMT+0100', 'Thu Jan 15 2026 10:00:00 GMT+0100 (CET)', 'Jan 15 2026 10:00 UTC', 'Jan 15 2026 10:00 UT',
  'January 15, 2026 10:00:00', 'January 15, 2026', '15 January 2026 23:59', '2026/01/15 10:00', '1/15/2026', '1/15/2026 13:00',
  'Thu, 15 Jan 2026 10:00:00 GMT', 'Thu, 15 Jan 2026 10:00:00', '2100-07-01T12:00:00', '2100-07-01 12:00', ' 2026-01-15 ', ' 2026 ',
  'garbage', '', '   ', '2026-02-30', '2026-01-15T24:00:00', '2026-01-15T10:00:00.5', '2026-01-15T10:00:00.123456',
  'Jan 15 2026 10:00 (comment)', 'Jan 15 2026 10:00 GMTx', '2026-01-15T10:00:00 +01:00 (CET)', '1994-12-31T12:00:00', '1986-01-01T00:10:00',
  '2026-04-05T02:15:00', '2026-10-04T02:15:00', '2026-11-01T01:30:00',
];
const parsed = texts.map((s) => [attempt(() => Date.parse(s)), attempt(() => new Date(s).getTime())]);
const singles = [
  () => new Date(['2024-01-01']).getTime(), () => new Date([5]).getTime(), () => new Date([]).getTime(), () => new Date({}).getTime(),
  () => new Date({ valueOf: () => 86400000 }).getTime(), () => new Date({ toString: () => '2024-03-01T00:00:00' }).getTime(),
  () => new Date({ [Symbol.toPrimitive]: (hint) => hint === 'default' ? '2024-03-01 10:00' : 0 }).getTime(),
  () => new Date({ [Symbol.toPrimitive]: () => ({}) }).getTime(), () => new Date({ valueOf: () => ({}), toString: () => ({}) }).getTime(),
  () => new Date({ [Symbol.toPrimitive]: 5, valueOf: () => 7 }).getTime(), () => new Date(Object.create(null)).getTime(),
  () => new Date(null).getTime(), () => new Date(undefined).getTime(), () => new Date(true).getTime(), () => new Date('').getTime(),
  () => new Date(new Date(1234)).getTime(), () => new Date(2020n).getTime(), () => new Date(Symbol('x')).getTime(),
  () => new Date(1.9).getTime(), () => new Date(-1.9).getTime(), () => new Date(8.64e15 + 1).getTime(), () => new Date(-0).getTime(),
  () => Date.parse(Symbol('x')), () => Date.parse(), () => Date.parse(12), () => Date.parse({ toString: () => '2026-01-15T10:00:00' }),
  () => new Date().getTime(), () => new Date(undefined, undefined).getTime(), () => Date(), () => Date(2020, 1), () => typeof Date(),
  () => { class Later extends Date { constructor(...a) { super(...a); } } const d = new Later(2026, 2, 29, 2, 30); return [d instanceof Later, d.getTime(), d.getHours(), d.toString()].join(' '); },
  () => Reflect.construct(Date, [2026, 0, 1], Array).getTime(), () => Object.prototype.toString.call(Reflect.construct(Date, [0], Array)),
  () => Date.prototype.getHours.call({}), () => Date.prototype.setHours.call({}, 1), () => Date.prototype.toString.call(1), () => Date.prototype.toLocaleString.call(null),
  () => Date.prototype.getTimezoneOffset.call(new Date(NaN)), () => new Date(NaN).toLocaleString(), () => new Date(NaN).toDateString(), () => new Date(NaN).toTimeString(),
  () => new Date(NaN).setMonth(1), () => new Date(NaN).setFullYear(2026), () => new Date(NaN).setFullYear(2026, 5), () => new Date(0).setHours(),
  () => new Date(0).toLocaleString(undefined, { timeZone: { toString: () => 'Asia/Tokyo' } }),
  () => new Date(0).toLocaleString(undefined, { timeZone: Symbol('x') }), () => new Date(0).toLocaleString(undefined, 'abc'),
  () => new Date(0).toLocaleString(undefined, { get timeZone() { throw new Error('zone getter'); } }),
  () => new Date(0).toLocaleString(undefined, { timeZone: 'UTC' }, 'extra'), () => new Date(0).toLocaleString(undefined, { timeZone: 'Europe/Berlin' }),
  () => new Date(0).toLocaleString(undefined, { timeZone: 'GMT' }), () => new Date(0).toLocaleString(undefined, { timeZone: 'Etc/GMT-14' }),
  () => new Date(0).toLocaleString(undefined, { timeZone: '' }), () => new Date(0).toLocaleString(undefined, { timeZone: 'EUROPE/BERLIN' }),
  () => Date.now(), () => Date.length, () => Date.name, () => Date.UTC(2026, 0),
  () => { const d = new Date(0); Date.prototype.getFullYear = function () { return 42; }; return d.getYear(); },
];
const single = singles.map((f) => attempt(f));
const out = { rows, made, order, parsed, single };
"#;

fn graph(content: Value) -> Arc<DecisionContent> {
    Arc::new(
        serde_json::from_value(json!({
            "nodes": [
                {"id": "in", "name": "in", "type": "inputNode"},
                {"id": "node", "name": "node", "type": "functionNode", "content": content},
                {"id": "out", "name": "out", "type": "outputNode"}
            ],
            "edges": [
                {"id": "e1", "sourceId": "in", "targetId": "node", "type": "edge"},
                {"id": "e2", "sourceId": "node", "targetId": "out", "type": "edge"}
            ]
        }))
        .unwrap(),
    )
}

fn modern() -> Arc<DecisionContent> {
    graph(json!({"source": format!(
        "export const handler = async (input) => {{ {SCRIPT} return {{ out: JSON.stringify(out) }}; }};"
    )}))
}

fn legacy() -> Arc<DecisionContent> {
    graph(json!(format!(
        "const handler = (input) => {{ {SCRIPT} return {{ out: JSON.stringify(out) }}; }}"
    )))
}

fn run(content: Arc<DecisionContent>, zone: &str) -> Value {
    let decision = DecisionEngine::default()
        .with_version(EngineVersion::V2)
        .create_decision(content)
        .unwrap();
    let options = EvaluationOptions {
        now: parse_instant("2026-03-29T00:45:00.250Z"),
        timezone: parse_timezone(zone),
        seed: Some(7),
        ..Default::default()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime
        .block_on(decision.evaluate_with_opts(Variable::from(json!({})), options))
        .unwrap();
    let text = response.result.to_value()["out"]
        .as_str()
        .unwrap()
        .to_string();
    let mut value: Value = serde_json::from_str(&text).unwrap();
    let rows: Vec<Value> = value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let mut fields = row.as_object().unwrap().clone();
            let t = fields.remove("t").unwrap();
            let sets = fields.remove("sets").unwrap();
            json!([t, digest(&Value::Object(fields)), digest(&sets)])
        })
        .collect();
    value["rows"] = Value::Array(rows);
    value
}

fn digest(value: &Value) -> String {
    let hash = value
        .to_string()
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(0x100000001b3)
        });
    format!("{hash:016x}")
}

fn mismatches(path: &str, want: &Value, got: &Value, found: &mut Vec<String>) {
    if found.len() > 40 {
        return;
    }
    match (want, got) {
        (Value::Object(a), Value::Object(b)) if a.len() == b.len() => {
            for (key, value) in a {
                match b.get(key) {
                    Some(other) => mismatches(&format!("{path}.{key}"), value, other, found),
                    None => found.push(format!("{path}.{key} missing")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                mismatches(&format!("{path}[{i}]"), x, y, found);
            }
        }
        _ if want != got => found.push(format!("{path}\n  want: {want}\n  got:  {got}")),
        _ => {}
    }
}

#[test]
fn pinned_dates_match_the_recorded_results_in_every_zone() {
    let path = format!("{}/{FIXTURE}", env!("CARGO_MANIFEST_DIR"));
    let (modern, legacy) = (modern(), legacy());
    let results: BTreeMap<String, Value> = ZONES
        .iter()
        .map(|zone| (zone.to_string(), run(modern.clone(), zone)))
        .collect();
    let mut found = Vec::new();
    for zone in ["Europe/Berlin", "Pacific/Kiritimati"] {
        let old = run(legacy.clone(), zone);
        mismatches(&format!("legacy {zone}"), &results[zone], &old, &mut found);
    }
    if std::env::var("PINNED_DATES_WRITE").is_ok_and(|v| v == "1") {
        let mut text = String::from("{\n");
        for (i, (zone, value)) in results.iter().enumerate() {
            let rows = value["rows"].as_array().unwrap();
            let rest: serde_json::Map<String, Value> = value
                .as_object()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k != "rows")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            text.push_str(&format!("  {}: {{\n    \"rows\": [\n", json!(zone)));
            for (j, row) in rows.iter().enumerate() {
                let comma = if j + 1 < rows.len() { "," } else { "" };
                text.push_str(&format!("      {row}{comma}\n"));
            }
            text.push_str("    ],\n");
            let keys: Vec<_> = rest.iter().collect();
            for (j, (key, value)) in keys.iter().enumerate() {
                let comma = if j + 1 < keys.len() { "," } else { "" };
                text.push_str(&format!("    {}: {value}{comma}\n", json!(key)));
            }
            let comma = if i + 1 < results.len() { "," } else { "" };
            text.push_str(&format!("  }}{comma}\n"));
        }
        text.push_str("}\n");
        std::fs::write(&path, text).unwrap();
    }
    let recorded: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (zone, got) in &results {
        mismatches(zone, &recorded[zone], got, &mut found);
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

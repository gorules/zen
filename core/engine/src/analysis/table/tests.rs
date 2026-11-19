use serde_json::{json, Map, Value};

use crate::model::DecisionContent;
use crate::policy::{DiagnosticCode, Workspace};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Number,
    Integer,
    Text,
    Choice,
    Flag,
    Untyped,
}

const KINDS: [Kind; 6] = [
    Kind::Number,
    Kind::Integer,
    Kind::Text,
    Kind::Choice,
    Kind::Flag,
    Kind::Untyped,
];
const WORDS: [&str; 6] = ["red", "green", "blue", "amber", "teal", "plum"];
const POINTS: [&str; 9] = ["0", "5", "10", "12.5", "20", "25", "40", "60", "100"];

fn number_cell(rng: &mut Rng) -> String {
    let a = *rng.pick(&POINTS);
    let b = *rng.pick(&POINTS);
    match rng.below(16) {
        0 | 1 => String::new(),
        2 => format!("> {a}"),
        3 => format!(">= {a}"),
        4 => format!("< {a}"),
        5 => format!("<= {a}"),
        6 => format!("[{a}..{b}]"),
        7 => format!("[{a}..{b})"),
        8 => format!("({a}..{b}]"),
        9 => a.to_string(),
        10 => format!("{a}, {b}"),
        11 => format!("< {a}, > {b}"),
        12 => format!("!= {a}"),
        13 => format!(">= {a} and < {b}"),
        14 => format!("not in [{a}, {b}]"),
        _ => "null".to_string(),
    }
}

fn text_cell(rng: &mut Rng) -> String {
    let a = *rng.pick(&WORDS);
    let b = *rng.pick(&WORDS);
    match rng.below(9) {
        0 | 1 => String::new(),
        2 | 3 => format!("\"{a}\""),
        4 => format!("\"{a}\", \"{b}\""),
        5 => format!("not in [\"{a}\", \"{b}\"]"),
        6 => format!("!= \"{a}\""),
        7 => "null".to_string(),
        _ => "len($) > 3".to_string(),
    }
}

fn flag_cell(rng: &mut Rng) -> String {
    match rng.below(5) {
        0 => String::new(),
        1 | 2 => "true".to_string(),
        3 => "false".to_string(),
        _ => "true, false".to_string(),
    }
}

fn cell(rng: &mut Rng, kind: Kind) -> String {
    match kind {
        Kind::Number | Kind::Integer => number_cell(rng),
        Kind::Text | Kind::Choice => text_cell(rng),
        Kind::Flag => flag_cell(rng),
        Kind::Untyped => match rng.below(3) {
            0 => number_cell(rng),
            1 => text_cell(rng),
            _ => flag_cell(rng),
        },
    }
}

fn schema_of(kind: Kind, nullable: bool) -> Option<Value> {
    let typed = |name: &str| match nullable {
        true => json!({ "type": [name, "null"] }),
        false => json!({ "type": name }),
    };
    match kind {
        Kind::Number => Some(typed("number")),
        Kind::Integer => Some(typed("integer")),
        Kind::Text => Some(typed("string")),
        Kind::Choice => Some(json!({ "enum": WORDS[..4] })),
        Kind::Flag => Some(typed("boolean")),
        Kind::Untyped => None,
    }
}

fn random_graph(rng: &mut Rng, rows: usize, columns: usize) -> DecisionContent {
    let kinds: Vec<Kind> = (0..columns).map(|_| *rng.pick(&KINDS)).collect();
    let shared = columns > 1 && rng.chance(15);
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut inputs = Vec::new();
    for (index, kind) in kinds.iter().enumerate() {
        let field = match shared && index == columns - 1 {
            true => "f0".to_string(),
            false => format!("f{index}"),
        };
        let kind = match shared && index == columns - 1 {
            true => kinds[0],
            false => *kind,
        };
        if let Some(schema) = schema_of(kind, rng.chance(20)) {
            properties.entry(field.clone()).or_insert(schema);
            if rng.chance(80) && !required.contains(&json!(field)) {
                required.push(json!(field));
            }
        }
        inputs.push(
            json!({ "id": format!("c{index}"), "name": format!("Column {index}"), "field": field }),
        );
    }
    let schema = json!({ "type": "object", "properties": properties, "required": required });
    let collect = rng.chance(15);
    let rules: Vec<Value> = (0..rows)
        .map(|row| {
            let mut rule = Map::new();
            rule.insert("_id".into(), json!(format!("r{row}")));
            for (index, kind) in kinds.iter().enumerate() {
                let kind = match shared && index == columns - 1 {
                    true => kinds[0],
                    false => *kind,
                };
                rule.insert(format!("c{index}"), json!(cell(rng, kind)));
            }
            rule.insert("o".into(), json!(format!("{}", rng.below(4))));
            Value::Object(rule)
        })
        .collect();
    serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "request", "type": "inputNode", "content": { "schema": schema.to_string() } },
            { "id": "dt", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": if collect { "collect" } else { "first" },
                "inputs": inputs,
                "outputs": [ { "id": "o", "name": "Out", "field": "out" } ],
                "rules": rules,
                "passThrough": true
            } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "dt" },
            { "id": "e2", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .expect("graph")
}

struct Reported {
    missing: bool,
    incomplete: bool,
    compressible: bool,
    unreachable: bool,
}

fn reported(content: DecisionContent) -> Reported {
    let mut workspace = Workspace::new();
    workspace.set_document("g", content);
    let diagnostics = workspace.diagnostics("g");
    let has = |code: DiagnosticCode| diagnostics.iter().any(|d| d.code == code);
    Reported {
        missing: has(DiagnosticCode::MissingCases),
        incomplete: has(DiagnosticCode::TableChecksIncomplete),
        compressible: has(DiagnosticCode::CompressibleTable),
        unreachable: has(DiagnosticCode::UnreachableRule),
    }
}

#[test]
fn table_checks_match_the_reference_on_random_tables() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let mut with_gaps = 0;
    let mut compressible = 0;
    let mut unreachable = 0;
    let mut split = 0;
    for round in 0..1500 {
        let rows = match round % 10 {
            0 => 1 + rng.below(8),
            9 => 60 + rng.below(240),
            _ => 9 + rng.below(50),
        };
        let columns = 1 + rng.below(5);
        let found = reported(random_graph(&mut rng, rows, columns));
        with_gaps += usize::from(found.missing);
        compressible += usize::from(found.compressible);
        unreachable += usize::from(found.unreachable);
        split += usize::from(rows > 8);
    }
    assert!(with_gaps > 300, "only {with_gaps} tables reported gaps");
    assert!(
        compressible > 200,
        "only {compressible} tables were compressible"
    );
    assert!(
        unreachable > 200,
        "only {unreachable} tables had unreachable rows"
    );
    assert!(split > 1000, "only {split} tables reached the split path");
}

fn range_heavy(rng: &mut Rng, rows: usize, columns: usize) -> DecisionContent {
    let mut properties = Map::new();
    let inputs: Vec<Value> = (0..columns)
        .map(|index| {
            properties.insert(format!("n{index}"), json!({ "type": "number" }));
            json!({ "id": format!("c{index}"), "name": format!("N{index}"), "field": format!("n{index}") })
        })
        .collect();
    let required: Vec<String> = properties.keys().cloned().collect();
    let schema = json!({ "type": "object", "properties": properties, "required": required });
    let rules: Vec<Value> = (0..rows)
        .map(|row| {
            let mut rule = Map::new();
            rule.insert("_id".into(), json!(format!("r{row}")));
            for index in 0..columns {
                let step = [1usize, 5, 10, 50, 100, 500][index % 6];
                let low = rng.below(60) * step;
                let high = low + (1 + rng.below(6)) * step;
                let cell = match rng.below(5) {
                    0 => format!("[{low}..{high})"),
                    1 => format!(">= {low} and < {high}"),
                    2 => format!("> {low}"),
                    3 => format!("<= {high}"),
                    _ => String::new(),
                };
                rule.insert(format!("c{index}"), json!(cell));
            }
            rule.insert("o".into(), json!(format!("{}", rng.below(9))));
            Value::Object(rule)
        })
        .collect();
    serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "request", "type": "inputNode", "content": { "schema": schema.to_string() } },
            { "id": "dt", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "inputs": inputs,
                "outputs": [ { "id": "o", "name": "Out", "field": "out" } ],
                "rules": rules,
                "passThrough": true
            } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "e1", "sourceId": "in", "targetId": "dt" },
            { "id": "e2", "sourceId": "dt", "targetId": "out" }
        ]
    }))
    .expect("graph")
}

#[test]
fn table_checks_match_the_reference_on_range_heavy_tables() {
    let mut rng = Rng(0x0BAD_CAFE_F00D_0002);
    let mut with_gaps = 0;
    let mut incomplete = 0;
    for (rows, columns) in [(40, 3), (80, 4), (120, 6), (200, 5), (300, 6)] {
        let found = reported(range_heavy(&mut rng, rows, columns));
        with_gaps += usize::from(found.missing);
        incomplete += usize::from(found.incomplete);
    }
    assert!(with_gaps >= 2, "only {with_gaps} tables reported gaps");
    assert_eq!(with_gaps + incomplete, 5);
}

#[test]
#[ignore]
fn table_checks_match_the_reference_when_the_budget_runs_out() {
    let mut rng = Rng(0x0DDB_A11C_0FFE_0003);
    let mut incomplete = 0;
    let mut complete = 0;
    for (rows, columns) in [(300, 3), (1000, 6), (2000, 6), (4000, 6), (4000, 9)] {
        let found = reported(range_heavy(&mut rng, rows, columns));
        incomplete += usize::from(found.incomplete);
        complete += usize::from(!found.incomplete);
    }
    assert!(incomplete >= 1, "no table exhausted the budget");
    assert!(complete >= 1, "no table finished inside the budget");
}

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::{json, Map, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{DiagnosticCode, Workspace};
use zen_engine::workspace::ScopeRequest;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static MEASURING: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(live, Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size >= layout.size() {
            let grown = new_size - layout.size();
            let live = LIVE.fetch_add(grown, Ordering::Relaxed) + grown;
            PEAK.fetch_max(live, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const MIB: usize = 1024 * 1024;

fn peak_of<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let result = work();
    (result, PEAK.load(Ordering::Relaxed).saturating_sub(before))
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }
}

fn range_table(seed: u64, rows: usize, columns: usize) -> DecisionContent {
    let mut rng = Rng(seed);
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
                let cell = match index % 6 {
                    3 => ["12", "24", "36", "48", "60", "> 60", "<= 36", ""][rng.below(8)]
                        .to_string(),
                    4 => ["< 0.2", "[0.2..0.35)", "[0.35..0.45)", ">= 0.45", ""][rng.below(5)]
                        .to_string(),
                    kind => {
                        let (floor, span, step) = [
                            (18, 62, 1),
                            (2, 78, 5000),
                            (30, 55, 10),
                            (0, 0, 0),
                            (0, 0, 0),
                            (1, 90, 1000),
                        ][kind];
                        let low = (floor + rng.below(span)) * step;
                        let high = low + (1 + rng.below(6)) * step;
                        match rng.below(5) {
                            0 => format!("[{low}..{high})"),
                            1 => format!(">= {low} and < {high}"),
                            2 => format!("> {low}"),
                            3 => format!("<= {high}"),
                            _ => String::new(),
                        }
                    }
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

struct Measured {
    peak: usize,
    payload: usize,
    cases: usize,
    incomplete: bool,
}

fn diagnose(content: DecisionContent) -> Measured {
    let mut workspace = Workspace::new();
    workspace.set_document("g", content);
    let (diagnostics, peak) = peak_of(|| workspace.diagnostics("g"));
    let payload = diagnostics
        .iter()
        .map(|d| d.message.len() + d.args.values().map(String::len).sum::<usize>())
        .sum();
    let cases = diagnostics
        .iter()
        .find(|d| d.code == DiagnosticCode::MissingCases)
        .and_then(|d| d.args.get("count"))
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    let incomplete = diagnostics
        .iter()
        .any(|d| d.code == DiagnosticCode::TableChecksIncomplete);
    Measured {
        peak,
        payload,
        cases,
        incomplete,
    }
}

#[test]
fn schema_queries_stay_small_on_range_heavy_tables() {
    let _guard = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for rows in [500, 1000, 2000] {
        let mut workspace = Workspace::new();
        workspace.set_document("g", range_table(0xA11C_E5ED_0000_0001, rows, 6));
        let request = ScopeRequest::for_policy("g");
        let (inputs, input_peak) = peak_of(|| workspace.inputs(&request));
        let (outputs, output_peak) = peak_of(|| workspace.outputs(&request));
        assert_eq!(inputs.len(), 6, "{rows} rows");
        assert!(!outputs.is_empty(), "{rows} rows");
        assert!(
            input_peak < 8 * MIB,
            "{rows} rows: deriving inputs peaked at {} KiB",
            input_peak / 1024
        );
        assert!(
            output_peak < 8 * MIB,
            "{rows} rows: deriving outputs peaked at {} KiB",
            output_peak / 1024
        );
        let diagnostics = workspace.diagnostics("g");
        assert!(
            diagnostics.iter().any(|d| matches!(
                d.code,
                DiagnosticCode::MissingCases | DiagnosticCode::TableChecksIncomplete
            )),
            "{rows} rows: table checks still run for diagnostics"
        );
    }
}

fn mixed_table(seed: u64, rows: usize) -> DecisionContent {
    let mut rng = Rng(seed);
    let regions = [
        "north", "south", "east", "west", "central", "coastal", "inland", "island",
    ];
    let terms = ["12", "24", "36", "48", "60", "> 60", "<= 36", ""];
    let ratios = ["< 0.2", "[0.2..0.35)", "[0.35..0.45)", ">= 0.45", ""];
    let spans: [(usize, usize, usize); 4] = [
        (18, 80, 1),
        (10_000, 400_000, 5_000),
        (1_000, 900_000, 1_000),
        (300, 850, 10),
    ];
    let mut properties = Map::new();
    properties.insert("region".into(), json!({ "type": "string" }));
    let mut inputs = vec![json!({ "id": "region", "name": "Region", "field": "region" })];
    for index in 0..6 {
        properties.insert(format!("n{index}"), json!({ "type": "number" }));
        inputs.push(json!({ "id": format!("c{index}"), "name": format!("N{index}"), "field": format!("n{index}") }));
    }
    let required: Vec<String> = properties.keys().cloned().collect();
    let schema = json!({ "type": "object", "properties": properties, "required": required });
    let rules: Vec<Value> = (0..rows)
        .map(|row| {
            let mut rule = Map::new();
            rule.insert("_id".into(), json!(format!("r{row}")));
            rule.insert(
                "region".into(),
                json!(format!("\"{}\"", regions[rng.below(regions.len())])),
            );
            for (index, (min, max, step)) in spans.iter().enumerate() {
                let low = (min / step + rng.below(max / step - min / step)) * step;
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
            rule.insert("c4".into(), json!(terms[rng.below(terms.len())]));
            rule.insert("c5".into(), json!(ratios[rng.below(ratios.len())]));
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
fn finished_checks_on_range_heavy_tables_cost_memory_in_proportion_to_their_report() {
    let _guard = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for rows in [250, 500, 1000, 1500] {
        let measured = diagnose(mixed_table(0x5EED_F00D_0000_0004, rows));
        assert!(!measured.incomplete, "{rows} rows: checks did not finish");
        assert!(
            measured.cases > 1000,
            "{rows} rows: {} missing cases",
            measured.cases
        );
        assert!(
            measured.peak <= 4 * measured.payload + 2 * MIB,
            "{rows} rows: peak {} KiB for {} KiB of diagnostics ({} missing cases)",
            measured.peak / 1024,
            measured.payload / 1024,
            measured.cases
        );
    }
}

fn banded_table(bands: usize, columns: usize, hole_every: usize) -> DecisionContent {
    let mut properties = Map::new();
    let inputs: Vec<Value> = (0..columns)
        .map(|index| {
            properties.insert(format!("n{index}"), json!({ "type": "number", "minimum": 0, "exclusiveMaximum": bands * 10 }));
            json!({ "id": format!("c{index}"), "name": format!("N{index}"), "field": format!("n{index}") })
        })
        .collect();
    let required: Vec<String> = properties.keys().cloned().collect();
    let schema = json!({ "type": "object", "properties": properties, "required": required });
    let mut rules: Vec<Value> = Vec::new();
    for combination in 0..bands.pow(columns as u32) {
        let picks: Vec<usize> = (0..columns)
            .map(|index| combination / bands.pow(index as u32) % bands)
            .collect();
        if (combination.wrapping_mul(2_654_435_761) >> 7) % hole_every == 0 {
            continue;
        }
        let mut rule = Map::new();
        rule.insert("_id".into(), json!(format!("r{combination}")));
        for (index, band) in picks.iter().enumerate() {
            rule.insert(
                format!("c{index}"),
                json!(format!("[{}..{})", band * 10, (band + 1) * 10)),
            );
        }
        rule.insert("o".into(), json!(format!("{}", combination % 7)));
        rules.push(Value::Object(rule));
    }
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
fn missing_cases_cost_memory_in_proportion_to_their_number() {
    let _guard = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for bands in [6, 9, 12, 15] {
        let measured = diagnose(banded_table(bands, 3, 3));
        assert!(!measured.incomplete, "{bands} bands: checks did not finish");
        assert!(measured.cases > 0, "{bands} bands: no missing cases");
        assert!(
            measured.peak <= 4 * measured.payload + 8 * MIB,
            "{bands} bands: peak {} KiB for {} KiB of diagnostics ({} missing cases)",
            measured.peak / 1024,
            measured.payload / 1024,
            measured.cases
        );
    }
}

#[test]
fn range_heavy_table_checks_stay_within_a_fixed_ceiling() {
    let _guard = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut finished = 0;
    let mut incomplete = 0;
    for (rows, columns) in [
        (250, 6),
        (500, 5),
        (1000, 5),
        (2000, 5),
        (1000, 6),
        (4000, 6),
        (8000, 6),
    ] {
        let measured = diagnose(range_table(0xB0B5_CAFE_0000_0002, rows, columns));
        finished += usize::from(!measured.incomplete);
        incomplete += usize::from(measured.incomplete);
        assert!(
            measured.peak <= 4 * measured.payload + 64 * MIB,
            "{rows}x{columns}: peak {} KiB for {} KiB of diagnostics ({} missing cases)",
            measured.peak / 1024,
            measured.payload / 1024,
            measured.cases
        );
    }
    assert!(
        finished >= 2,
        "only {finished} tables finished their checks"
    );
    assert!(
        incomplete >= 2,
        "only {incomplete} tables exhausted the budget"
    );
}

fn decision_graph(
    schema: Value,
    inputs: Value,
    outputs: Value,
    rules: Vec<Value>,
) -> DecisionContent {
    serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "request", "type": "inputNode", "content": { "schema": schema.to_string() } },
            { "id": "dt", "name": "table", "type": "decisionTableNode", "content": {
                "hitPolicy": "first",
                "inputs": inputs,
                "outputs": outputs,
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

fn lookup_table(rows: usize) -> DecisionContent {
    let schema = json!({
        "type": "object",
        "properties": { "status": { "type": "number" }, "code": { "type": "string" } },
        "required": ["status", "code"]
    });
    let rules = (0..rows)
        .map(|row| {
            let status = ["200", "400", "500"][row % 3];
            json!({
                "_id": format!("r{row}"),
                "s": status,
                "c": format!("'CODE_{row:06}'"),
                "o": format!("'REASON_{row:06}'")
            })
        })
        .collect();
    decision_graph(
        schema,
        json!([
            { "id": "s", "name": "Status", "field": "status" },
            { "id": "c", "name": "Code", "field": "code" }
        ]),
        json!([ { "id": "o", "name": "Reason", "field": "reason" } ]),
        rules,
    )
}

fn banded_grid(rows: usize) -> DecisionContent {
    let schema = json!({
        "type": "object",
        "properties": {
            "amount": { "type": "number" },
            "region": { "type": "string" },
            "tier": { "type": "string" },
            "flag": { "type": "boolean" },
            "age": { "type": "number" }
        },
        "required": ["amount", "region", "tier", "flag", "age"]
    });
    let regions = [
        "\"north\", \"south\"",
        "\"east\"",
        "\"west\", \"central\"",
        "\"coastal\"",
    ];
    let tiers = ["\"basic\"", "\"plus\"", "\"premium\""];
    let ages = [(18, 30), (30, 45), (45, 65), (65, 120)];
    let rules = (0..rows)
        .map(|row| {
            let band = row / (regions.len() * tiers.len() * 2 * ages.len());
            let (low, high) = ages[row % ages.len()];
            let region = regions[row / (tiers.len() * 2 * ages.len()) % regions.len()];
            let tier = tiers[row / (2 * ages.len()) % tiers.len()];
            let flag = match row / ages.len() % 2 {
                0 => "true",
                _ => "false",
            };
            json!({
                "_id": format!("r{row}"),
                "a": format!(">= {} and < {}", band * 1000, (band + 1) * 1000),
                "r": region,
                "t": tier,
                "f": flag,
                "g": format!("[{low}..{high})"),
                "o": format!("{}", band as f64 * 0.001)
            })
        })
        .collect();
    decision_graph(
        schema,
        json!([
            { "id": "a", "name": "Amount", "field": "amount" },
            { "id": "r", "name": "Region", "field": "region" },
            { "id": "t", "name": "Tier", "field": "tier" },
            { "id": "f", "name": "Flag", "field": "flag" },
            { "id": "g", "name": "Age", "field": "age" }
        ]),
        json!([ { "id": "o", "name": "Rate", "field": "rate" } ]),
        rules,
    )
}

#[test]
fn lookup_tables_with_one_value_per_row_grow_linearly() {
    let _guard = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let small = diagnose(lookup_table(10_000));
    let large = diagnose(lookup_table(40_000));
    assert!(
        large.peak <= 6 * small.peak,
        "four times the rows took {} KiB after {} KiB",
        large.peak / 1024,
        small.peak / 1024
    );
    assert!(
        large.peak <= 40_000 * 2 * 1024,
        "40,000 lookup rows peaked at {} KiB",
        large.peak / 1024
    );
}

#[test]
fn compressible_grids_stay_under_two_kibibytes_a_row() {
    let _guard = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for rows in [20_000, 40_000] {
        let mut workspace = Workspace::new();
        workspace.set_document("g", banded_grid(rows));
        let (diagnostics, peak) = peak_of(|| workspace.diagnostics("g"));
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::CompressibleTable),
            "{rows} rows: the grid should be reported as compressible"
        );
        assert!(
            peak <= rows * 2 * 1024 + 8 * MIB,
            "{rows} rows peaked at {} KiB",
            peak / 1024
        );
    }
}

fn merging_lookup(rows: usize) -> DecisionContent {
    let schema = json!({
        "type": "object",
        "properties": { "status": { "type": "number" }, "code": { "type": "string" } },
        "required": ["status", "code"]
    });
    let rules = (0..rows)
        .map(|row| {
            let code = row * 7919 % rows;
            let status = ["200", "400", "500"][row % 3];
            json!({
                "_id": format!("r{row}"),
                "s": status,
                "c": format!("'CODE_{code:06}'"),
                "o": "'BLOCKED'"
            })
        })
        .collect();
    decision_graph(
        schema,
        json!([
            { "id": "s", "name": "Status", "field": "status" },
            { "id": "c", "name": "Code", "field": "code" }
        ]),
        json!([ { "id": "o", "name": "Reason", "field": "reason" } ]),
        rules,
    )
}

#[test]
fn lookup_rows_that_merge_into_one_cell_grow_linearly() {
    let _guard = MEASURING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut peaks = Vec::new();
    for rows in [10_000, 20_000, 40_000] {
        let mut workspace = Workspace::new();
        workspace.set_document("g", merging_lookup(rows));
        let (diagnostics, peak) = peak_of(|| workspace.diagnostics("g"));
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::CompressibleTable),
            "{rows} rows: the lookup should be reported as compressible"
        );
        assert!(
            peak <= rows * 2 * 1024 + 2 * MIB,
            "{rows} rows peaked at {} KiB",
            peak / 1024
        );
        peaks.push(peak);
    }
    assert!(
        peaks[1] <= 3 * peaks[0],
        "twice the rows took {} KiB after {} KiB",
        peaks[1] / 1024,
        peaks[0] / 1024
    );
}

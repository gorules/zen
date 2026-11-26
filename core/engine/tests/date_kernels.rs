mod conformance;
mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use zen_engine::context::{parse_instant, parse_timezone};
use zen_engine::policy::{EvaluateRequest, PolicyWorkspace};
use zen_engine::{diagnostics, EngineVersion, EvaluationOptions};

const PATH: &str = "main";
const UNITS: [&str; 8] = ["year", "quarter", "month", "week", "day", "hour", "minute", "second"];
const ZONES: [&str; 8] = [
    "UTC",
    "America/New_York",
    "Europe/Berlin",
    "Asia/Kolkata",
    "Australia/Lord_Howe",
    "Pacific/Chatham",
    "America/Sao_Paulo",
    "Etc/GMT-14",
];

fn workspace(text: &str, version: EngineVersion) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new().with_version(version);
    let literal = conformance::literal::Parser::document(text).expect("policy literal");
    let document = conformance::policy::Policy::expand(&literal[0]).expect("policy expand");
    ws.set_policy(PATH, serde_json::from_value(document).expect("policy json"));
    ws
}

fn policy() -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut add = |value: String| {
        let at = blocks.len();
        blocks.push(format!("{{expression: \"c.o{at}\", value: \"{value}\"}}"));
    };
    for unit in UNITS {
        add(format!("c.a.diff(c.b, '{unit}')"));
        add(format!("d().diff(c.a, '{unit}')"));
        add(format!("c.b.diff(c.t, '{unit}')"));
        add(format!("c.a.diff('2024-02-29T12:00:00', '{unit}')"));
        for method in ["isSame", "isBefore", "isAfter", "isSameOrBefore", "isSameOrAfter"] {
            add(format!("c.a.{method}(c.b, '{unit}')"));
        }
        add(format!("c.a.startOf('{unit}').diff(c.b.endOf('{unit}'), 'minute')"));
        add(format!("c.a.add(3, '{unit}').diff(c.a.sub(1, '{unit}'), 'hour')"));
    }
    add("d().add(-24, 'hour').diff(c.a, 'minute')".into());
    add("c.a.add(1, 'day').diff(c.b, 'hour')".into());
    add("c.a.diff(c.b)".into());
    add("c.a.diff(c.n)".into());
    for method in ["isSame", "isBefore", "isAfter", "isSameOrBefore", "isSameOrAfter"] {
        add(format!("c.a.{method}(c.b)"));
        add(format!("c.b.{method}(c.t)"));
        add(format!("d().{method}(c.a, 'day')"));
    }
    for pattern in ["%Y-%m-%d", "%H:%M:%S", "%Y-%m-%dT%H:%M:%S%:z", "%j %A %B"] {
        add(format!("c.a.format('{pattern}')"));
    }
    add("c.b.format()".into());
    for method in ["isValid", "isLeapYear", "isToday", "isYesterday", "isTomorrow"] {
        add(format!("c.a.{method}()"));
        add(format!("d(c.t).{method}()"));
    }
    for method in ["second", "minute", "hour", "day", "dayOfYear", "week", "weekday", "month", "quarter", "year", "timestamp"] {
        add(format!("c.a.{method}() + c.b.{method}()"));
    }
    for zone in ["America/New_York", "Asia/Kolkata", "UTC", "Australia/Lord_Howe"] {
        add(format!("c.a.tz('{zone}').hour() * 100 + c.a.tz('{zone}').minute()"));
        add(format!("c.b.tz('{zone}').format('%Y-%m-%d %H:%M')"));
        add(format!("c.a.tz('{zone}').diff(c.b, 'month')"));
    }
    format!(
        "{{\n  models: {{c: {{a: \"date\", b: \"date\", t: \"string\", n: \"number\"}}}},\n  blocks: [\n    {}\n  ],\n}}",
        blocks.join(",\n    ")
    )
}

fn dates() -> Vec<&'static str> {
    vec![
        "2024-02-29",
        "2023-02-28",
        "2024-01-31",
        "2024-03-31",
        "2024-12-31T23:59:59",
        "2025-01-01T00:00:00",
        "2024-03-10T02:30:00",
        "2024-03-10T01:59:59",
        "2024-03-10T03:00:00",
        "2024-11-03T01:30:00",
        "2024-11-03T00:30:00",
        "2024-03-31T02:30:00",
        "2024-10-27T02:30:00",
        "2024-04-07T01:45:00",
        "2024-10-06T02:15:00",
        "2024-09-29T02:50:00",
        "2025-03-19",
        "2025-03-20",
        "2025-03-20T10:15:30Z",
        "2025-03-20T11:15:30+01:00",
        "2025-03-21T23:59:59",
        "2025-03-20T23:30:00-05:00",
        "2025-03-21T04:30:00+05:30",
        "1970-01-01",
        "1999-12-31T23:59:59Z",
        "2000-02-29T12:00:00+13:45",
        "2038-01-19T03:14:08Z",
        "2099-06-15T06:00:00",
        "2024-06-30T12:00:00",
        "2024-07-31",
        "2024-08-31T18:30:00",
    ]
}

fn rows() -> Vec<Value> {
    let dates = dates();
    let mut rows = Vec::new();
    for (i, a) in dates.iter().enumerate() {
        for step in [1usize, 5, 7, 11, 13, 17, 23] {
            let b = dates[(i + step) % dates.len()];
            let t = dates[(i * 3 + step) % dates.len()];
            rows.push(json!({"c": {"a": a, "b": b, "t": t, "n": (i * step) as i64 * 86_400_000}}));
        }
    }
    let distinct = rows.len();
    (0..distinct * 24).map(|at| rows[(at / 3) % distinct].clone()).collect()
}

#[test]
fn typed_date_methods_match_the_general_functions_in_every_zone() {
    let text = policy();
    let rows = rows();
    let built = Built::new(&rows).expect("columns");
    assert_eq!(built.rows, rows.len());
    let columns = built.columns();
    let path: Arc<str> = Arc::from(PATH);
    let new = workspace(&text, EngineVersion::V2);
    for zone in ZONES {
        let options = EvaluationOptions {
            now: parse_instant("2025-03-20T10:15:30Z"),
            timezone: parse_timezone(zone),
            ..Default::default()
        };
        let run = |typed: bool| {
            diagnostics::set_typed(typed);
            let output = new
                .evaluate_columns_pinned(&path, &[], &columns, &options)
                .expect("columnar policy");
            let paths = output.paths();
            let results: Vec<Result<Value, String>> = (0..columns.rows)
                .map(|at| match output.errors[at].as_deref() {
                    Some(error) => Err(format!("{error:?}")),
                    None => Ok(Built::normalized(output.row(at).to_value())),
                })
                .collect();
            (paths, results)
        };
        let (typed_paths, typed) = run(true);
        let (_, general) = run(false);
        diagnostics::set_typed(true);
        assert!(typed_paths.typed > 0 && typed_paths.hosted == 0, "{zone}: {typed_paths:?}");
        for (at, (got, want)) in typed.iter().zip(&general).enumerate() {
            assert_eq!(got, want, "zone {zone} row {at}: input {}", rows[at]);
        }
    }
    let old = workspace(&text, EngineVersion::V1);
    let options = EvaluationOptions {
        now: parse_instant("2025-03-20T10:15:30Z"),
        ..Default::default()
    };
    diagnostics::set_typed(true);
    let output = new
        .evaluate_columns_pinned(&path, &[], &columns, &options)
        .expect("columnar policy");
    for at in 0..columns.rows {
        let request = EvaluateRequest {
            policy_path: path.clone(),
            input: columns.row(at).depth_clone(usize::MAX),
            goals: Vec::new(),
            trace: zen_engine::trace::TraceFormat::Off,
        };
        let want = old
            .evaluate_pinned(&request, &options)
            .map(|result| Built::normalized(result.output.to_value()))
            .map_err(|error| support::golden::Outcome::debug(&error));
        let got = match output.errors[at].as_deref() {
            Some(error) => Err(support::policy_error(error)
                .map_or_else(|| format!("{error:?}"), support::golden::Outcome::debug)),
            None => Ok(Built::normalized(output.row(at).to_value())),
        };
        assert_eq!(got, want, "row {at}: input {}", rows[at]);
    }
}

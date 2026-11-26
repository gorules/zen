#[allow(dead_code)]
mod conformance;
mod support;

use serde_json::{json, Value};
use std::sync::Arc;
use support::columns::Built;
use support::golden::Outcome;
use zen_engine::diagnostics::Paths;
use zen_engine::policy::{EvaluateRequest, PolicyWorkspace};
use zen_engine::EngineVersion;
use zen_expression::lane::{Column, Columns, Dictionary, Values};
use zen_expression::Variable;

const PATH: &str = "main";

fn workspace(policies: &[(&str, &str)], version: EngineVersion) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new().with_version(version);
    for (path, text) in policies {
        let literal = conformance::literal::Parser::document(text).expect("policy literal");
        let document = conformance::policy::Policy::expand(&literal[0]).expect("policy expand");
        ws.set_policy(
            *path,
            serde_json::from_value(document).expect("policy json"),
        );
    }
    ws
}

fn request(input: Variable) -> EvaluateRequest {
    EvaluateRequest {
        policy_path: Arc::from(PATH),
        input,
        goals: Vec::new(),
        trace: zen_engine::trace::TraceFormat::Off,
    }
}

fn options() -> zen_engine::EvaluationOptions {
    zen_engine::EvaluationOptions {
        now: zen_engine::context::parse_instant("2025-03-20T10:15:30Z"),
        ..Default::default()
    }
}

fn columns_match_version_one(policies: &[(&str, &str)], rows: Vec<Value>) -> Paths {
    let old = workspace(policies, EngineVersion::V1);
    let new = workspace(policies, EngineVersion::V2);
    let repeated: Vec<Value> = rows
        .iter()
        .cycle()
        .take((rows.len() * 8).max(32))
        .cloned()
        .collect();
    let paths = columns_match(&old, &new, &rows);
    columns_match(&old, &new, &repeated);
    paths
}

fn columns_match(old: &PolicyWorkspace, new: &PolicyWorkspace, rows: &[Value]) -> Paths {
    let built = Built::nested(rows)
        .or_else(|| Built::lists(rows))
        .or_else(|| Built::new(rows))
        .expect("columns");
    assert_eq!(built.rows, rows.len(), "rows kept by the column builder");
    let fields = built.fields();
    let structs = built.structs(&fields);
    let columns = built.columns_with(&structs);
    given_columns_match(old, new, &columns)
}

fn given_columns_match(old: &PolicyWorkspace, new: &PolicyWorkspace, columns: &Columns) -> Paths {
    goals_match(old, new, columns, &[])
}

fn goals_match(
    old: &PolicyWorkspace,
    new: &PolicyWorkspace,
    columns: &Columns,
    goals: &[Arc<str>],
) -> Paths {
    let rows = 0..columns.rows;
    let path: Arc<str> = Arc::from(PATH);
    let output = new
        .evaluate_columns_pinned(&path, goals, columns, &options())
        .expect("columnar policy");
    for at in rows.clone() {
        let input = columns.row(at);
        let asked = EvaluateRequest {
            goals: goals.to_vec(),
            ..request(input.depth_clone(usize::MAX))
        };
        let want = old
            .evaluate_pinned(&asked, &options())
            .map(|result| Built::normalized(result.output.to_value()))
            .map_err(|error| Outcome::debug(&error));
        let got =
            match output.errors[at].as_deref() {
                Some(error) => Err(support::policy_error(error)
                    .map_or_else(|| format!("{error:?}"), Outcome::debug)),
                None => Ok(Built::normalized(output.row(at).to_value())),
            };
        assert_eq!(
            got,
            want,
            "row {at} of {}: input {}",
            rows.len(),
            input.to_value()
        );
    }
    output.paths()
}

fn columns_match_document(document: Value, rows: Vec<Value>) -> Paths {
    let workspace = |version| {
        let mut ws = PolicyWorkspace::new().with_version(version);
        ws.set_policy(PATH, serde_json::from_value(document.clone()).expect("policy json"));
        ws
    };
    let (old, new) = (workspace(EngineVersion::V1), workspace(EngineVersion::V2));
    columns_match(&old, &new, &rows)
}

fn typed_like_version_one(policies: &[(&str, &str)], rows: Vec<Value>) -> Paths {
    let paths = columns_match_version_one(policies, rows);
    assert!(
        paths.typed > 0 && paths.laned == 0 && paths.hosted == 0,
        "expected typed, got {paths:?}"
    );
    paths
}

const DATED: &str = r#"{
  models: {contract: {start: "date", end: "date?", value: "number", kind: "string"}},
  blocks: [
    {expression: "contract.big", value: "contract.value > 1000"},
    {expression: "contract.fee", value: "contract.kind == 'lease' ? contract.value * 0.1 : 5"},
  ],
}"#;

#[test]
fn date_inputs_are_validated_by_columns() {
    let rows: Vec<Value> = (0..96)
        .map(|i| {
            let mut contract = json!({
                "start": format!("2024-{:02}-{:02}", 1 + i % 12, 1 + i % 28),
                "value": 100 * (i % 17),
                "kind": (["lease", "service", "rental"])[i % 3],
            });
            if i % 4 == 0 {
                contract["end"] = json!(format!("2026-01-{:02}", 1 + i % 28));
            }
            json!({"contract": contract})
        })
        .collect();
    typed_like_version_one(&[(PATH, DATED)], rows);
}

#[test]
fn date_inputs_mixed_with_invalid_and_empty_text_match_version_one() {
    let starts = [
        json!("2024-02-29"),
        json!(""),
        json!("not a date"),
        json!("2024-13-45"),
        json!("2025-03-20T10:15:30Z"),
        json!("2025-03-20T23:30:00+05:00"),
        Value::Null,
        json!(20240101),
    ];
    let rows: Vec<Value> = (0..64)
        .map(|i| {
            let mut contract = json!({"value": 10 * i, "kind": "lease"});
            if !starts[i % starts.len()].is_null() {
                contract["start"] = starts[i % starts.len()].clone();
            }
            if i % 3 == 0 {
                contract["end"] = starts[(i + 3) % starts.len()].clone();
            }
            json!({"contract": contract})
        })
        .collect();
    columns_match_version_one(&[(PATH, DATED)], rows);
}

const INPUT_ARMS: &str = r#"{
  models: {customer: {income: "number", tier: "string", age: "number", bonus: "number?"}},
  blocks: [
    {match: "customer.limit", arms: [
      ["customer.tier == 'gold'", "customer.income * 3"],
      ["customer.age > 40", "customer.income * 2 + (customer.bonus ?? 0)"],
      ["", "customer.income"],
    ]},
    {table: "fees", inputs: ["customer.tier"], outputs: ["customer.fee"], rules: [
      ["'gold'", "customer.income / 100"],
      ["'silver'", "customer.age + 5"],
      ["", "1"],
    ]},
    {expression: "customer.net", value: "customer.limit - customer.fee"},
  ],
}"#;

#[test]
fn match_arms_and_table_cells_reading_only_inputs_run_typed() {
    let tiers = ["gold", "silver", "bronze"];
    let rows: Vec<Value> = (0..150)
        .map(|i| {
            let mut customer =
                json!({"income": 1000 + 37 * i, "tier": tiers[i % 3], "age": 20 + i % 50});
            if i % 4 == 0 {
                customer["bonus"] = json!(i);
            }
            json!({"customer": customer})
        })
        .collect();
    typed_like_version_one(&[(PATH, INPUT_ARMS)], rows);
}

#[test]
fn match_arms_reading_inputs_keep_errors_of_the_chosen_arm_only() {
    let policy = r#"{
      models: {customer: {income: "number", tier: "string", code: "string"}},
      blocks: [
        {match: "customer.limit", arms: [
          ["customer.tier == 'gold'", "customer.income / (customer.income - 1000)"],
          ["", "len(customer.code) * customer.income"],
        ]},
      ],
    }"#;
    let rows: Vec<Value> = (0..64)
        .map(|i| json!({"customer": {"income": 990 + 5 * (i % 5), "tier": (["gold", "silver"])[i % 2], "code": "ab"}}))
        .collect();
    columns_match_version_one(&[(PATH, policy)], rows);
}

const DATE_READS: &str = r#"{
  models: {contract: {start: "date", end: "date?", value: "number", kind: "string", events: "event[]"}, event: {at: "date", amount: "number"}},
  blocks: [
    {expression: "contract.startYear", value: "contract.start.year()"},
    {expression: "contract.days", value: "contract.end == null ? 0 : contract.end.diff(contract.start, 'day')"},
    {expression: "contract.early", value: "contract.start < d('2024-06-01')"},
    {expression: "contract.renewal", value: "contract.start.add(1, 'year').format('%Y-%m-%d')"},
    {expression: "event.month", value: "event.at.month()"},
    {expression: "event.late", value: "event.at > contract.start.add(30, 'day')"},
    {expression: "contract.lateEvents", value: "count(contract.events, #.late)"},
    {table: "phase", inputs: ["contract.start", "contract.kind"], outputs: ["contract.phase"], rules: [
      ["< d('2024-03-01')", "'lease'", "'old-lease'"],
      ["< d('2024-09-01')", "", "'mid'"],
      ["", "", "'new'"],
    ]},
  ],
}"#;

fn date_rows(count: usize, starts: &[Value]) -> Vec<Value> {
    (0..count)
        .map(|i| {
            let events: Vec<Value> = (0..i % 4)
                .map(|j| json!({"at": format!("2024-{:02}-{:02}", 1 + (i + j) % 12, 1 + j * 7), "amount": j * 10}))
                .collect();
            let mut contract = json!({
                "value": 100 * (i % 17),
                "kind": (["lease", "service", "rental"])[i % 3],
                "events": events,
            });
            if !starts[i % starts.len()].is_null() {
                contract["start"] = starts[i % starts.len()].clone();
            }
            if i % 4 != 1 {
                contract["end"] = json!(format!("2026-{:02}-{:02}", 1 + i % 12, 1 + i % 28));
            }
            json!({"contract": contract})
        })
        .collect()
}

#[test]
fn declared_date_inputs_run_typed() {
    let starts: Vec<Value> = (0..40)
        .map(|i| json!(format!("2024-{:02}-{:02}", 1 + i % 12, 1 + i % 28)))
        .collect();
    let paths = columns_match_version_one(&[(PATH, DATE_READS)], date_rows(120, &starts));
    assert!(
        paths.typed >= 7 && paths.hosted == 0,
        "expected typed date reads, got {paths:?}"
    );
}

#[test]
fn declared_date_inputs_with_offsets_empty_text_and_nulls_match_version_one() {
    let starts = [
        json!("2024-02-29"),
        json!(""),
        json!("2025-03-20T10:15:30Z"),
        json!("2025-03-20T23:30:00+05:00"),
        json!("2024-12-31T23:59:59-08:00"),
        Value::Null,
        json!("2024-03-31 02:30"),
        json!("2024-07-04T12:00:00.250Z"),
    ];
    columns_match_version_one(&[(PATH, DATE_READS)], date_rows(96, &starts));
}

#[test]
fn declared_dates_in_every_spelling_match_version_one() {
    let starts = [
        json!("2024"),
        json!("2024-02"),
        json!("2024-02-29"),
        json!("2023-02-29"),
        json!("20240229"),
        json!("2024/02/29"),
        json!("2024/02/29 10:11"),
        json!("2024/02/29 10:11:12.5"),
        json!("20240229T101112"),
        json!("20240229T1011"),
        json!("2024-03-31T02:30:00"),
        json!("2024-03-31 02:30:00"),
        json!("2024-10-27 02:30:00"),
        json!("2024-02-29T10:11:12.123456789Z"),
        json!("2024-02-29T10:11:12+0530"),
        json!("2024-02-29T10:11:12+05"),
        json!("2024-02-29T10:11Z"),
        json!("2024-02-29t10:11:12z"),
        json!("2024-02-29T24:00:00Z"),
        json!("0000-01-01"),
        json!("9999-12-31T23:59:59Z"),
        json!("10000-01-01"),
        json!("2024-1-5"),
        json!("2024-01-05 "),
        json!("2024-01-05x"),
        json!("Europe/Berlin"),
        json!("now"),
        json!("2024-06-01"),
        json!("2024-06-01T00:00:00Z"),
    ];
    for start in &starts {
        let one = std::slice::from_ref(start);
        columns_match_version_one(&[(PATH, DATE_READS)], date_rows(12, one));
    }
    columns_match_version_one(&[(PATH, DATE_READS)], date_rows(174, &starts));
}

#[test]
fn more_distinct_declared_dates_than_are_remembered_match_version_one() {
    let rows: Vec<Value> = (0..9_000usize)
        .map(|i| {
            let at = |n: usize| {
                let seconds = 1_700_000_000 + (n as i64) * 7_919;
                let day = seconds.div_euclid(86_400);
                let rest = seconds.rem_euclid(86_400);
                format!(
                    "2024-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                    1 + day % 12,
                    1 + day % 28,
                    rest / 3_600,
                    rest / 60 % 60,
                    rest % 60
                )
            };
            let events: Vec<Value> = (0..i % 3)
                .map(|j| json!({"at": at(i * 3 + j + 50_000), "amount": j * 10}))
                .collect();
            json!({"contract": {
                "start": at(i),
                "end": at(i + 20_000),
                "value": 100 * (i % 17),
                "kind": (["lease", "service", "rental"])[i % 3],
                "events": events,
            }})
        })
        .collect();
    let policies = [(PATH, DATE_READS)];
    let old = workspace(&policies, EngineVersion::V1);
    let new = workspace(&policies, EngineVersion::V2);
    let paths = columns_match(&old, &new, &rows);
    assert!(paths.hosted == 0, "{paths:?}");
}

const DATED_LISTS: &str = r#"{
  models: {
    contract: {start: "date", events: "event[]"},
    event: {at: "date?", amount: "number", notes: "note[]"},
    note: {on: "date", text: "string"},
  },
  blocks: [
    {expression: "contract.recent", value: "count(contract.events, #.at != null and #.at > d('2024-06-01'))"},
    {expression: "contract.sameYear", value: "len(filter(contract.events, #.at != null and #.at.year() == contract.start.year()))"},
    {expression: "contract.months", value: "map(filter(contract.events, #.at != null), #.at.month())"},
    {expression: "contract.total", value: "sum(map(contract.events, #.amount))"},
    {expression: "event.fresh", value: "count(event.notes, d('2025-01-01').diff(#.on, 'month') < 6)"},
    {expression: "event.noted", value: "some(event.notes, #.on > d('2024-09-01') and #.text == 'late')"},
    {expression: "contract.fresh", value: "sum(map(contract.events, #.fresh))"},
  ],
}"#;

const DATED_LISTS_SHOWN: &str = r#"{
  models: {
    contract: {start: "date", events: "event[]"},
    event: {at: "date?", amount: "number", notes: "note[]"},
    note: {on: "date", text: "string"},
  },
  blocks: [
    {expression: "contract.recent", value: "count(contract.events, #.at != null and #.at > d('2024-06-01'))"},
    {expression: "contract.dates", value: "map(contract.events, #.at)"},
    {expression: "contract.late", value: "filter(contract.events, #.at != null and #.at > d('2024-06-01'))"},
    {expression: "contract.firstAt", value: "len(contract.events) > 0 ? contract.events[0].at : null"},
    {expression: "contract.shown", value: "map(filter(contract.events, #.at != null), string(#.at))"},
    {expression: "event.fresh", value: "count(event.notes, d('2025-01-01').diff(#.on, 'month') < 6)"},
    {expression: "event.dates", value: "map(event.notes, #.on)"},
    {expression: "event.last", value: "max(map(event.notes, #.on))"},
  ],
}"#;

const DATE_ARRAYS: &str = r#"{
  models: {customer: {logins: "date[]", joined: "date"}},
  blocks: [
    {expression: "customer.thisYear", value: "filter(customer.logins, #.year() == 2025)"},
    {expression: "customer.first", value: "len(customer.logins) > 0 ? customer.logins[0] : null"},
    {expression: "customer.since", value: "count(customer.logins, # > customer.joined)"},
    {expression: "customer.years", value: "map(customer.logins, #.year())"},
  ],
}"#;

#[test]
fn arrays_of_declared_dates_match_version_one() {
    let spellings = [
        "2025-01-02",
        "2024-05-05",
        "2025/03/01",
        "2025-03-20T10:15:30Z",
        "2023",
    ];
    let rows: Vec<Value> = (0..60)
        .map(|i| {
            let logins: Vec<Value> = (0..i % 5)
                .map(|j| json!(spellings[(i + j) % spellings.len()]))
                .collect();
            json!({"customer": {"logins": logins, "joined": format!("2024-{:02}-15", 1 + i % 12)}})
        })
        .collect();
    columns_match_version_one(&[(PATH, DATE_ARRAYS)], rows);
}

fn dated_list_rows(count: usize, spellings: &[Value]) -> Vec<Value> {
    let pick = |n: usize| spellings[n % spellings.len()].clone();
    (0..count)
        .map(|i| {
            let events: Vec<Value> = (0..i % 4)
                .map(|j| {
                    let notes: Vec<Value> = (0..(i + j) % 3)
                        .map(|k| {
                            let on = pick(i * 5 + j * 3 + k);
                            let on = match on.is_null() {
                                true => json!("2024-11-30"),
                                false => on,
                            };
                            json!({"on": on, "text": (["late", "early"])[(i + k) % 2]})
                        })
                        .collect();
                    let mut event = json!({"amount": 10 * j + i % 7, "notes": notes});
                    let at = pick(i * 7 + j);
                    if !at.is_null() {
                        event["at"] = at;
                    }
                    event
                })
                .collect();
            json!({"contract": {
                "start": format!("2024-{:02}-{:02}", 1 + i % 12, 1 + i % 28),
                "events": events,
            }})
        })
        .collect()
}

#[test]
fn declared_dates_inside_lists_run_typed() {
    let spellings: Vec<Value> = (0..37)
        .map(|i| {
            json!(format!(
                "202{}-{:02}-{:02}",
                3 + i % 3,
                1 + i % 12,
                1 + i % 28
            ))
        })
        .collect();
    let paths = columns_match_version_one(&[(PATH, DATED_LISTS)], dated_list_rows(120, &spellings));
    assert!(
        paths.typed >= 7 && paths.laned == 0 && paths.hosted == 0,
        "expected typed list dates, got {paths:?}"
    );
}

#[test]
fn declared_dates_inside_lists_in_every_spelling_match_version_one() {
    let spellings = [
        json!("2024-02-29"),
        Value::Null,
        json!("2025-03-20T10:15:30Z"),
        json!("2025-03-20T23:30:00+05:00"),
        json!("2024-12-31T23:59:59-08:00"),
        json!("2024-03-31 02:30"),
        json!("2024-07-04T12:00:00.250Z"),
        json!("2024"),
        json!("2024-06"),
        json!("20240229"),
        json!("2024/06/02"),
        json!("2024-06-01"),
        json!("2024-06-01T00:00:00Z"),
        json!("2024-09-01"),
    ];
    for policy in [DATED_LISTS, DATED_LISTS_SHOWN] {
        columns_match_version_one(&[(PATH, policy)], dated_list_rows(140, &spellings));
        for odd in [json!(""), json!("soon"), json!("2023-02-29"), json!(7)] {
            let mut mixed = spellings.to_vec();
            mixed.insert(3, odd);
            columns_match_version_one(&[(PATH, policy)], dated_list_rows(90, &mixed));
        }
    }
}

#[test]
fn dated_lists_whose_rows_fail_or_leave_the_columns_match_version_one() {
    let policy = r#"{
      models: {
        contract: {start: "date", events: "event[]"},
        event: {at: "date?", amount: "number", notes: "note[]"},
        note: {on: "date", text: "string"},
      },
      blocks: [
        {expression: "event.year", value: "event.at != null ? event.at.year() : 0"},
        {expression: "event.fresh", value: "count(event.notes, d('2025-01-01').diff(#.on, 'month') < 6)"},
        {expression: "contract.latest", value: "max(map(contract.events, #.year))"},
        {expression: "contract.copy", value: "contract.events"},
        {expression: "contract.firstNote", value: "contract.events[0].notes[0].on"},
        {expression: "contract.span", value: "contract.latest - contract.start.year()"},
      ],
    }"#;
    let spellings = [
        json!("2024-02-29"),
        Value::Null,
        json!("2025-03-20T23:30:00+05:00"),
        json!("2024/06/02"),
        json!("2023"),
    ];
    let mut rows = dated_list_rows(80, &spellings);
    rows.push(json!({"contract": {"start": "2024-01-01", "events": null}}));
    rows.push(json!({"contract": {"start": "2024-01-01"}}));
    rows.push(json!({"contract": {"start": "never", "events": []}}));
    rows.push(json!({"contract": {"start": "2024-01-01", "events": [{"at": "soon", "amount": 1, "notes": []}]}}));
    rows.push(json!({"contract": {"start": "2024-01-01", "events": [{"amount": 1, "notes": [{"on": "2024-13-01", "text": "x"}]}]}}));
    columns_match_version_one(&[(PATH, policy)], rows);
}

const CHECKED_LISTS: &str = r#"{
  models: {
    order: {kind: "string(web|store)", placed: "date?", lines: "line[]"},
    line: {kind: "string(goods|service)", on: "date?", qty: "number", parts: "part[]", tags: "string(a|b)[]"},
    part: {grade: "string(new|used)", made: "date?"},
  },
  blocks: [
    {expression: "line.double", value: "line.qty * 2"},
    {expression: "order.goods", value: "count(order.lines, #.kind == 'goods')"},
    {expression: "order.total", value: "sum(map(order.lines, #.double))"},
    {expression: "order.label", value: "order.kind + '-' + string(order.goods)"},
    {expression: "order.year", value: "order.placed != null ? order.placed.year() : 0"},
    {expression: "order.early", value: "count(order.lines, #.on != null and #.on < d('2025-01-01'))"},
  ],
}"#;

#[test]
fn values_a_model_refuses_inside_lists_report_what_version_one_reports() {
    let kinds = [json!("goods"), json!("service"), json!("rental"), json!(3), Value::Null];
    let dates = [json!("2024-02-29"), json!("soon"), json!("2023-02-30"), Value::Null, json!(""), json!(20240101)];
    let grades = [json!("new"), json!("used"), json!("broken"), json!(false)];
    let tags = [json!("a"), json!("b"), json!("c"), json!(1)];
    for (bad_kind, bad_date, bad_grade, bad_tag) in [(0, 0, 0, 0), (7, 0, 0, 0), (0, 5, 0, 0), (0, 0, 9, 0), (0, 0, 0, 11), (13, 17, 19, 23)] {
        let odd = |every: usize, n: usize| every > 0 && n % every == every - 1;
        let pick = |pool: &[Value], bad: bool, n: usize| match bad {
            true => pool[2 + n % (pool.len() - 2)].clone(),
            false => pool[n % 2].clone(),
        };
        let rows: Vec<Value> = (0..96)
            .map(|i| {
                let lines: Vec<Value> = (0..i % 4)
                    .map(|j| {
                        let n = i * 4 + j;
                        let parts: Vec<Value> = (0..(i + j) % 3)
                            .map(|k| json!({
                                "grade": pick(&grades, odd(bad_grade, n * 3 + k), n + k),
                                "made": pick(&dates[..4], odd(bad_date, n * 3 + k + 1), 0),
                            }))
                            .collect();
                        let line_tags: Vec<Value> = (0..(i + j) % 3)
                            .map(|k| pick(&tags, odd(bad_tag, n * 3 + k), n + k))
                            .collect();
                        json!({
                            "kind": pick(&kinds, odd(bad_kind, n), n),
                            "on": pick(&dates, odd(bad_date, n), 0),
                            "qty": 1 + n % 5,
                            "parts": parts,
                            "tags": line_tags,
                        })
                    })
                    .collect();
                json!({"order": {"kind": (["web", "store"])[i % 2], "placed": "2025-01-05", "lines": lines}})
            })
            .collect();
        columns_match_version_one(&[(PATH, CHECKED_LISTS)], rows);
    }
}

#[test]
fn dictionary_columns_inside_lists_are_checked_like_plain_ones() {
    fn text(values: &[&str]) -> (Vec<i32>, String) {
        let mut offsets = vec![0i32];
        let mut data = String::new();
        for value in values {
            data.push_str(value);
            offsets.push(data.len() as i32);
        }
        (offsets, data)
    }
    let policies = [(PATH, CHECKED_LISTS)];
    let old = workspace(&policies, EngineVersion::V1);
    let new = workspace(&policies, EngineVersion::V2);
    for (kind_pool, date_pool, root_pool) in [
        (vec!["goods", "service"], vec!["2024-02-29", "2025-03-20T10:15:30Z", ""], vec!["web", "store"]),
        (vec!["goods", "service", "rental"], vec!["2024-02-29", ""], vec!["web", "store"]),
        (vec!["goods", "service"], vec!["2024-02-29", "soon", "2023-02-30"], vec!["web", "store"]),
        (vec!["goods", "service"], vec!["2024-02-29"], vec!["web", "phone", "store"]),
    ] {
        let rows = 80usize;
        let mut offsets = vec![0i32];
        (0..rows).for_each(|i| offsets.push(offsets[i] + 1 + (i % 4) as i32));
        let items = offsets[rows] as usize;
        let (kind_offsets, kind_data) = text(&kind_pool);
        let (date_offsets, date_data) = text(&date_pool);
        let (root_offsets, root_data) = text(&root_pool);
        let kind_keys: Vec<i32> = (0..items)
            .map(|n| if n % 11 == 5 { -1 } else { ((n * 7 + n / 5) % kind_pool.len()) as i32 })
            .collect();
        let date_keys: Vec<i32> = (0..items).map(|n| ((n * 3 + n / 7) % date_pool.len()) as i32).collect();
        let root_keys: Vec<i32> = (0..rows).map(|i| ((i * 5 + i / 9) % root_pool.len()) as i32).collect();
        let mut date_valid = vec![u64::MAX; items.div_ceil(64)];
        (0..items)
            .filter(|n| n % 6 == 1)
            .for_each(|n| date_valid[n / 64] &= !(1 << (n % 64)));
        let qty: Vec<i64> = (0..items).map(|n| 1 + (n % 5) as i64).collect();
        let fields = [
            (
                "kind",
                Column::new(Values::Dict {
                    keys: &kind_keys,
                    values: Dictionary::Text { offsets: &kind_offsets, data: &kind_data },
                }),
            ),
            (
                "on",
                Column::with_validity(
                    Values::Dict {
                        keys: &date_keys,
                        values: Dictionary::Text { offsets: &date_offsets, data: &date_data },
                    },
                    &date_valid,
                    0,
                ),
            ),
            ("qty", Column::new(Values::I64(&qty))),
        ];
        let item = Column::new(Values::Struct { fields: &fields, len: items });
        let lines = Column::new(Values::List { offsets: &offsets, child: Dictionary::Column(&item) });
        let root = Column::new(Values::Dict {
            keys: &root_keys,
            values: Dictionary::Text { offsets: &root_offsets, data: &root_data },
        });
        let placed = Column::new(Values::Dict {
            keys: &date_keys[..rows],
            values: Dictionary::Text { offsets: &date_offsets, data: &date_data },
        });
        let columns = Columns::new(rows)
            .column("order.kind", root)
            .column("order.placed", placed)
            .column("order.lines", lines);
        given_columns_match(&old, &new, &columns);
    }
}

#[test]
fn declared_dates_of_objects_inside_list_items_match_version_one() {
    let policy = r#"{
      models: {
        order: {placed: "date", lines: "line[]"},
        line: {qty: "number", meta: "meta", due: "date?", stops: "stop[]"},
        meta: {since: "date?", kind: "string"},
        stop: {at: "date", place: "place"},
        place: {opened: "date?", name: "string"},
      },
      blocks: [
        {expression: "line.old", value: "line.meta.since != null and line.meta.since < d('2024-06-01')"},
        {expression: "line.wait", value: "line.due != null ? line.due.diff(order.placed, 'day') : 0"},
        {expression: "line.late", value: "count(line.stops, #.at > d('2025-01-01'))"},
        {expression: "order.years", value: "map(filter(order.lines, #.meta.since != null), #.meta.since.year())"},
        {expression: "order.old", value: "count(order.lines, #.old)"},
        {expression: "order.firstDue", value: "len(order.lines) > 0 ? order.lines[0].due : null"},
        {expression: "order.opened", value: "flatMap(order.lines, map(#.stops, #.place.opened))"},
        {expression: "order.waits", value: "sum(map(order.lines, #.wait))"},
      ],
    }"#;
    let dates = [
        json!("2024-02-29"),
        json!("2025-03-20T23:30:00+05:00"),
        Value::Null,
        json!("2023"),
        json!("2024/06/02"),
        json!("2025-02-01"),
    ];
    for (bad, empty) in [(0, 0), (0, 5), (17, 0)] {
        let date = |n: usize| match (bad > 0 && n % bad == bad - 1, empty > 0 && n % empty == empty - 1) {
            (true, _) => json!("soon"),
            (_, true) => json!(""),
            _ => dates[n % dates.len()].clone(),
        };
        let solid = |n: usize| match date(n) {
            Value::Null => json!("2024-11-30"),
            other => other,
        };
        let rows: Vec<Value> = (0..72)
            .map(|i| {
                let lines: Vec<Value> = (0..i % 4)
                    .map(|j| {
                        let n = i * 5 + j * 3;
                        let stops: Vec<Value> = (0..(i + j) % 3)
                            .map(|k| json!({"at": solid(n + k), "place": {"opened": date(n + k + 2), "name": "p"}}))
                            .collect();
                        let mut line = json!({"qty": 1 + n % 4, "meta": {"since": date(n), "kind": "k"}, "stops": stops});
                        if (i + j) % 3 != 0 {
                            line["due"] = date(n + 1);
                        }
                        line
                    })
                    .collect();
                json!({"order": {"placed": solid(i), "lines": lines}})
            })
            .collect();
        columns_match_version_one(&[(PATH, policy)], rows);
    }
}

#[test]
fn declared_dates_in_lists_without_member_blocks_stay_dates() {
    let policy = r#"{
      models: {
        trip: {start: "date", stops: "stop[]", legs: "leg[]"},
        stop: {at: "date?", city: "string"},
        leg: {km: "number", checks: "check[]"},
        check: {on: "date", ok: "boolean"},
      },
      blocks: [
        {expression: "leg.long", value: "leg.km > 100"},
        {expression: "leg.same", value: "count(leg.checks, #.on == '2024-02-29')"},
        {expression: "leg.listed", value: "count(leg.checks, #.on in ['2024-02-29', '2023'])"},
        {expression: "leg.after", value: "count(leg.checks, #.on > '2024-06-01')"},
        {expression: "leg.last", value: "max(map(leg.checks, #.on))"},
        {expression: "trip.same", value: "count(trip.stops, #.at == '2024-02-29')"},
        {expression: "trip.listed", value: "count(trip.stops, #.at in ['2024-02-29', '2023'])"},
        {expression: "trip.after", value: "count(trip.stops, #.at != null and #.at > '2024-06-01')"},
        {expression: "trip.dates", value: "map(trip.stops, #.at)"},
        {expression: "trip.first", value: "min(map(filter(trip.stops, #.at != null), #.at))"},
        {expression: "trip.begun", value: "count(trip.stops, #.at != null and #.at >= trip.start)"},
        {expression: "trip.early", value: "count(trip.stops, #.at != null and #.at < '2024-02-01')"},
        {expression: "trip.years", value: "map(filter(trip.stops, #.at != null), #.at.year())"},
      ],
    }"#;
    let dates = [json!("2024-02-29"), json!("2024/01/05"), json!("2025-03-20T23:30:00+05:00"), Value::Null, json!("2023")];
    let rows: Vec<Value> = (0..80)
        .map(|i| {
            let stops: Vec<Value> = (0..1 + i % 3)
                .map(|j| {
                    let mut stop = json!({"city": (["a", "b"])[(i + j) % 2]});
                    let at = dates[(i * 3 + j) % dates.len()].clone();
                    if !at.is_null() {
                        stop["at"] = at;
                    }
                    stop
                })
                .collect();
            let legs: Vec<Value> = (0..i % 3)
                .map(|j| {
                    let checks: Vec<Value> = (0..1 + (i + j) % 2)
                        .map(|k| json!({"on": dates[(i + j + k) % 3].clone(), "ok": k % 2 == 0}))
                        .collect();
                    json!({"km": 40 * (i + j) % 300, "checks": checks})
                })
                .collect();
            json!({"trip": {"start": "2024-03-01", "stops": stops, "legs": legs}})
        })
        .collect();
    let paths = columns_match_version_one(&[(PATH, policy)], rows);
    assert!(paths.typed > 0, "{paths:?}");
}

#[test]
fn declared_dates_shown_as_text_keep_their_source_text() {
    let policy = r#"{
      models: {contract: {start: "date", kind: "string"}},
      blocks: [
        {expression: "contract.copy", value: "contract.start"},
        {expression: "contract.label", value: "contract.kind + ' ' + string(contract.start)"},
        {expression: "contract.later", value: "contract.kind == 'x' ? contract.start : d('2024-01-01')"},
        {expression: "contract.year", value: "contract.start.year()"},
      ],
    }"#;
    let starts = [
        json!("2024-02-29"),
        json!("2025-03-20T23:30:00+05:00"),
        json!("2024-07-04 08:00"),
    ];
    let rows: Vec<Value> = (0..48)
        .map(|i| json!({"contract": {"start": starts[i % 3].clone(), "kind": (["x", "y"])[i % 2]}}))
        .collect();
    columns_match_version_one(&[(PATH, policy)], rows);
}

const SPARSE_OUTPUTS: &str = r#"{
  models: {user: {plan: "string", country: "string", age: "number", code: "string?"}},
  blocks: [
    {table: "perks", inputs: ["user.plan", "user.country", "user.age"], outputs: ["user.perk", "user.level", "user.note"], rules: [
      ["'enterprise'", "", "", "'sla'", "3", ""],
      ["'enterprise', 'business'", "'DE', 'NO'", "", "'eu-hosting'", "", "'eu'"],
      ["'pro', 'team'", "", "> 40", "", "2", ""],
      ["", "'US'", "", "'us-support'", "", "'us'"],
      ["", "", "< 18", "", "0", "'minor'"],
      ["", "", "", "'community'", "1", ""],
    ]},
    {expression: "user.score", value: "user.level * 10 + len(user.perk ?? '')"},
  ],
}"#;

fn sparse_rows(count: usize) -> Vec<Value> {
    let plans = ["enterprise", "business", "pro", "team", "free"];
    let countries = ["DE", "NO", "US", "GB"];
    (0..count)
        .map(|i| {
            let mut user = json!({
                "plan": plans[i % plans.len()],
                "country": countries[(i / 5) % countries.len()],
                "age": 10 + (i * 7) % 60,
            });
            if i % 3 == 0 {
                user["code"] = json!(format!("c{i}"));
            }
            json!({"user": user})
        })
        .collect()
}

#[test]
fn strict_tables_with_empty_output_cells_run_typed() {
    typed_like_version_one(&[(PATH, SPARSE_OUTPUTS)], sparse_rows(160));
}

#[test]
fn strict_tables_with_empty_output_cells_keep_errors_of_rules_before_the_last_pick() {
    let policy = r#"{
      models: {user: {plan: "string", age: "number", tag: "string"}},
      blocks: [
        {table: "t", inputs: ["user.plan", "user.tag"], outputs: ["user.a", "user.b"], rules: [
          ["'x'", "", "1", ""],
          ["", "> 5", "2", ""],
          ["", "", "", "3"],
        ]},
      ],
    }"#;
    let rows: Vec<Value> = (0..64)
        .map(|i| json!({"user": {"plan": (["x", "y"])[i % 2], "age": i, "tag": (["abc", "7"])[(i / 2) % 2]}}))
        .collect();
    columns_match_version_one(&[(PATH, policy)], rows);
}

const REFUSED_MEMBER_TABLE: &str = r#"{
  models: {customer: {income: "number", tier: "string", accounts: "account[]"}, account: {balance: "number", kind: "string"}},
  blocks: [
    {table: "risk", inputs: [null, "account.kind"], outputs: ["account.risk"], rules: [
      ["account.balance > customer.income", "'loan'", "'high'"],
      ["", "'loan'", "'mid'"],
      ["", "", "'low'"],
    ]},
    {expression: "customer.score", value: "customer.income / 100 + (customer.tier == 'gold' ? 5 : 0)"},
    {match: "customer.band", arms: [["customer.score > 50", "'a'"], ["", "'b'"]]},
    {expression: "customer.highRisk", value: "count(customer.accounts, #.risk == 'high')"},
  ],
}"#;

#[test]
fn a_member_table_the_lane_refuses_keeps_the_root_blocks_typed() {
    let kinds = ["loan", "savings", "checking"];
    let rows: Vec<Value> = (0..96)
        .map(|i| {
            let accounts: Vec<Value> = (0..i % 4)
                .map(|j| json!({"balance": 1000 * ((i + j) % 9), "kind": kinds[(i + j) % 3]}))
                .collect();
            json!({"customer": {"income": 2000 + 150 * (i % 30), "tier": (["gold", "silver"])[i % 2], "accounts": accounts}})
        })
        .collect();
    let paths = columns_match_version_one(&[(PATH, REFUSED_MEMBER_TABLE)], rows);
    assert!(paths.typed >= 2 && paths.hosted == 0, "{paths:?}");
}

const REFERENCED: &str = r#"{
  models: {
    order: {shipTo: "&jurisdiction", fee: "number", exempt: "boolean", lines: "line[]"},
    jurisdiction: {id: "string", stateRate: "number", localRate: "number", taxesShipping: "boolean", cap: "number?"},
    line: {amount: "number", code: "string"},
  },
  blocks: [
    {expression: "line.taxable", value: "line.code != 'food' and line.amount < (order.shipTo.cap ?? 1000000)"},
    {expression: "order.rate", value: "order.shipTo.stateRate + order.shipTo.localRate"},
    {expression: "order.shippingTax", value: "(order.exempt or not order.shipTo.taxesShipping) ? 0 : round(order.fee * order.rate, 2)"},
    {expression: "order.total", value: "sum(map(order.lines, #.amount)) + order.fee + order.shippingTax"},
    {match: "order.band", arms: [["order.rate > 0.08", "'high'"], ["", "'low'"]]},
  ],
}"#;

fn pool() -> Value {
    json!([
        {"id": "NY", "stateRate": 0.04, "localRate": 0.045, "taxesShipping": true, "cap": 110},
        {"id": "CA", "stateRate": 0.0725, "localRate": 0.0125, "taxesShipping": false},
        {"id": "TX", "stateRate": 0.0625, "localRate": 0.02, "taxesShipping": true},
    ])
}

fn referenced_rows(count: usize, keys: &[Value]) -> Vec<Value> {
    (0..count)
        .map(|i| {
            let lines: Vec<Value> = (0..i % 3)
                .map(|j| json!({"amount": 20 * (i + j) % 300, "code": (["food", "general"])[j % 2]}))
                .collect();
            let mut order = json!({"fee": 5 + i % 7, "exempt": i % 5 == 0, "lines": lines});
            if !keys[i % keys.len()].is_null() {
                order["shipTo"] = keys[i % keys.len()].clone();
            }
            json!({"order": order, "jurisdiction": pool()})
        })
        .collect()
}

#[test]
fn references_to_a_pool_resolve_by_columns_and_run_typed() {
    let keys = [json!("NY"), json!("CA"), json!("TX")];
    let paths = columns_match_version_one(&[(PATH, REFERENCED)], referenced_rows(120, &keys));
    assert!(paths.typed >= 4 && paths.hosted == 0, "{paths:?}");
}

#[test]
fn references_that_are_missing_unknown_or_not_text_match_version_one() {
    let keys = [json!("NY"), json!("XX"), Value::Null, json!(7), json!("CA"), json!("")];
    columns_match_version_one(&[(PATH, REFERENCED)], referenced_rows(72, &keys));
    let rows: Vec<Value> = (0..24)
        .map(|i| match i % 3 {
            0 => json!({"order": {"shipTo": "NY", "fee": 1, "exempt": false, "lines": []}}),
            1 => json!({"order": {"shipTo": "NY", "fee": 2, "exempt": false, "lines": []}, "jurisdiction": [{"id": "NY", "stateRate": 1, "localRate": 2, "taxesShipping": true}, {"id": "NY", "stateRate": 3, "localRate": 4, "taxesShipping": false}]}),
            _ => json!({"order": {"fee": 3, "exempt": true, "lines": []}, "jurisdiction": []}),
        })
        .collect();
    columns_match_version_one(&[(PATH, REFERENCED)], rows);
}

#[test]
fn rows_failing_in_two_blocks_report_the_error_version_one_reports() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/policy/delegated_error_order.json"
    );
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("json");
    let rows: Vec<Value> = fixture["rows"].as_array().expect("rows").clone();
    columns_match_document(fixture["policy"].clone(), rows);
}

#[test]
fn a_reference_missing_in_some_rows_stays_missing_when_read_whole() {
    let policy = r#"{
      models: {order: {shipTo: "&jurisdiction?", fee: "number"}, jurisdiction: {id: "string", stateRate: "number", note: "string?"}},
      blocks: [
        {expression: "order.shipped", value: "order.shipTo != null"},
        {match: "order.rate", arms: [["order.shipTo.note != null", "order.shipTo.stateRate * 2"], ["", "order.shipTo.stateRate ?? 0"]]},
      ],
    }"#;
    let rows: Vec<Value> = (0..40)
        .map(|i| {
            let mut order = json!({"fee": i});
            if i % 3 != 0 {
                order["shipTo"] = json!((["NY", "CA"])[i % 2]);
            }
            json!({"order": order, "jurisdiction": [{"id": "NY", "stateRate": 0.04, "note": "n"}, {"id": "CA", "stateRate": 0.07}]})
        })
        .collect();
    columns_match_version_one(&[(PATH, policy)], rows);
}

#[test]
fn nested_lists_of_objects_match_version_one() {
    let policy = r#"{
      models: {
        employer: {contribution: "number", employees: "employee[]"},
        employee: {dob: "date", salary: "number", dependents: "dependent[]"},
        dependent: {relation: "string(spouse|child)", dob: "date"},
      },
      blocks: [
        {expression: "employee.cover", value: "min([employee.salary * 3, 500000])"},
        {expression: "employee.deps", value: "count(employee.dependents, #.relation == 'spouse' or d().diff(#.dob, 'year') < 26)"},
        {expression: "employer.total", value: "sum(map(employer.employees, #.cover)) * employer.contribution"},
      ],
    }"#;
    let relations = ["spouse", "child", "cousin"];
    let rows: Vec<Value> = (0..60)
        .map(|i| {
            let employees: Vec<Value> = (0..i % 4)
                .map(|j| {
                    let dependents: Vec<Value> = (0..(i + j) % 3)
                        .map(|k| json!({"relation": relations[(i + k) % if i % 7 == 0 { 3 } else { 2 }], "dob": format!("{}-0{}-1{}", 1990 + k * 9, 1 + k, k)}))
                        .collect();
                    json!({"dob": format!("198{}-05-0{}", j, 1 + j), "salary": 30000 + 1000 * i, "dependents": dependents})
                })
                .collect();
            json!({"employer": {"contribution": 0.5, "employees": employees}})
        })
        .collect();
    columns_match_version_one(&[(PATH, policy)], rows);
}

#[test]
fn rows_failing_an_aggregate_over_an_empty_list_fail_alone() {
    let policy = r#"{
      models: {
        claim: {amount: "number", kind: "string", items: "item[]"},
        item: {price: "number", qty: "number", category: "string"},
      },
      blocks: [
        {expression: "item.total", value: "item.price * item.qty"},
        {table: "itemRisk", inputs: ["item.category", "item.total"], outputs: ["item.risk"], rules: [
          ["'jewelry', 'electronics'", "> 1000", "3"], ["'jewelry'", "", "2"], ["", "> 2000", "2"], ["", "", "0"],
        ]},
        {expression: "claim.itemsTotal", value: "sum(map(claim.items, #.total))"},
        {expression: "claim.maxRisk", value: "max(map(claim.items, #.risk))"},
        {match: "claim.severity", arms: [["claim.amount > 50000", "'major'"], ["claim.amount > 5000", "'moderate'"], ["", "'minor'"]]},
        {expression: "claim.score", value: "claim.maxRisk * 10 + (claim.kind == 'auto' ? 5 : 0)"},
      ],
    }"#;
    let categories = ["jewelry", "electronics", "tools"];
    let rows = |empty: usize| -> Vec<Value> {
        (0..96)
            .map(|i| {
                let count = if empty > 0 && i % empty == 0 { 0 } else { 1 + i % 3 };
                let items: Vec<Value> = (0..count)
                    .map(|j| json!({"price": 300 * (1 + (i + j) % 9), "qty": 1 + j, "category": categories[(i + j) % 3]}))
                    .collect();
                json!({"claim": {"amount": 1000 * (i % 70), "kind": (["auto", "home"])[i % 2], "items": items}})
            })
            .collect()
    };
    let paths = columns_match_version_one(&[(PATH, policy)], rows(5));
    assert!(paths.typed > 0 && paths.hosted > 0, "{paths:?}");
    columns_match_version_one(&[(PATH, policy)], rows(1));
    let paths = columns_match_version_one(&[(PATH, policy)], rows(0));
    assert!(paths.typed > 0 && paths.hosted == 0, "{paths:?}");
}

const SETTLEMENT: &str = r#"{
  models: {
    order: {country: "string", lines: "line[]", sellers: "seller[]"},
    line: {seller: "string", price: "number", qty: "number?", shipped: "date?", parts: "part[]"},
    part: {weight: "number"},
    seller: {id: "string", country: "string", rating: "number?", joined: "date?"},
  },
  blocks: [
    {expression: "seller.lines", value: "count(order.lines, #.seller == seller.id)"},
    {expression: "seller.gross", value: "sum(map(filter(order.lines, #.seller == seller.id), #.price * (#.qty ?? 1)))"},
    {expression: "seller.early", value: "some(order.lines, #.seller == seller.id and #.shipped != null and seller.joined != null and #.shipped < seller.joined)"},
    {expression: "seller.heavy", value: "count(order.lines, #.seller == seller.id and sum(map(#.parts, #.weight)) > 5)"},
    {expression: "seller.rank", value: "count(order.sellers, (#.rating ?? 0) > (seller.rating ?? 0)) + 1"},
    {expression: "seller.peers", value: "count(order.sellers, #.country == seller.country) - 1"},
    {expression: "seller.share", value: "seller.gross * 100 / (sum(map(order.sellers, #.gross)) + 1)"},
    {match: "seller.tier", arms: [["seller.rank == 1 and seller.lines > 0", "'top'"], ["seller.gross > 500", "'big'"], ["", "'plain'"]]},
    {expression: "order.total", value: "sum(map(order.sellers, #.gross))"},
    {expression: "order.tops", value: "count(order.sellers, #.tier == 'top')"},
  ],
}"#;

fn settlement_rows(count: usize, lines: impl Fn(usize) -> Option<usize>, sellers: impl Fn(usize) -> Option<usize>) -> Vec<Value> {
    let ids = ["s1", "s2", "s3", "s4"];
    let dates = ["2024-02-29", "2025-01-15", "2023-07-01"];
    (0..count)
        .map(|i| {
            let mut order = json!({"country": (["DE", "NO"])[i % 2]});
            if let Some(n) = lines(i) {
                let items: Vec<Value> = (0..n)
                    .map(|j| {
                        let parts: Vec<Value> = (0..(i + j) % 3).map(|k| json!({"weight": 1 + (i + k) % 5})).collect();
                        let mut line = json!({"seller": ids[(i + j * 3) % 4], "price": 50 * (1 + (i + j) % 9), "parts": parts});
                        if (i + j) % 3 != 0 {
                            line["qty"] = json!(1 + j % 3);
                        }
                        if (i + j) % 4 != 1 {
                            line["shipped"] = json!(dates[(i + j) % 3]);
                        }
                        line
                    })
                    .collect();
                order["lines"] = json!(items);
            }
            if let Some(n) = sellers(i) {
                let items: Vec<Value> = (0..n)
                    .map(|j| {
                        let mut seller = json!({"id": ids[(i + j) % 4], "country": (["DE", "NO", "SE"])[(i + j) % 3]});
                        if (i + j) % 5 != 0 {
                            seller["rating"] = json!((i * 7 + j * 3) % 5);
                        }
                        if (i + j) % 3 != 2 {
                            seller["joined"] = json!(dates[(i + 2 * j) % 3]);
                        }
                        seller
                    })
                    .collect();
                order["sellers"] = json!(items);
            }
            json!({"order": order})
        })
        .collect()
}

#[test]
fn member_blocks_reading_their_own_list_and_another_list_run_typed() {
    let rows = settlement_rows(120, |i| Some(i % 5), |i| Some(1 + i % 4));
    let paths = columns_match_version_one(&[(PATH, SETTLEMENT)], rows);
    assert!(paths.typed >= 8 && paths.hosted == 0, "{paths:?}");
}

#[test]
fn member_blocks_reading_lists_that_are_empty_null_or_missing_match_version_one() {
    let policies = [(PATH, SETTLEMENT)];
    columns_match_version_one(&policies, settlement_rows(60, |_| Some(0), |i| Some(i % 3)));
    columns_match_version_one(&policies, settlement_rows(60, |i| Some(i % 4), |_| Some(0)));
    columns_match_version_one(&policies, settlement_rows(60, |i| Some(i % 4), |_| Some(1)));
    columns_match_version_one(&policies, settlement_rows(60, |i| (i % 3 != 0).then_some(i % 4), |i| Some(1 + i % 3)));
    columns_match_version_one(&policies, settlement_rows(60, |i| Some(1 + i % 4), |i| (i % 4 != 1).then_some(i % 3)));
    columns_match_version_one(&policies, settlement_rows(40, |_| Some(7), |_| Some(9)));
    let mut rows = settlement_rows(40, |i| Some(i % 4), |i| Some(1 + i % 3));
    rows.push(json!({"order": {"country": "DE", "lines": null, "sellers": [{"id": "s1", "country": "DE"}]}}));
    rows.push(json!({"order": {"country": "DE", "lines": [{"seller": "s1", "price": 5, "parts": []}], "sellers": null}}));
    rows.push(json!({"order": {"country": "DE"}}));
    columns_match_version_one(&policies, rows);
    let mut rows = settlement_rows(40, |i| Some(1 + i % 4), |i| Some(1 + i % 3));
    rows[7]["order"]["lines"][0]["price"] = json!(98765432109876543210.0);
    rows[9]["order"]["sellers"][0]["rating"] = json!(98765432109876543210.0);
    columns_match_version_one(&policies, rows);
}

#[test]
fn member_blocks_reading_lists_keep_the_errors_of_failing_members() {
    let policy = r#"{
      models: {
        order: {lines: "line[]", sellers: "seller[]"},
        line: {seller: "string", price: "number"},
        seller: {id: "string", rating: "number?"},
      },
      blocks: [
        {expression: "seller.best", value: "max(map(filter(order.lines, #.seller == seller.id), #.price))"},
        {expression: "seller.top", value: "max(map(order.sellers, #.rating ?? 0)) == (seller.rating ?? 0)"},
        {expression: "seller.ratio", value: "seller.best / len(filter(order.sellers, #.id == seller.id))"},
        {expression: "order.best", value: "max(map(order.sellers, #.best))"},
      ],
    }"#;
    let ids = ["s1", "s2", "s3"];
    let rows: Vec<Value> = (0..90)
        .map(|i| {
            let lines: Vec<Value> = (0..i % 4)
                .map(|j| json!({"seller": ids[(i + j) % if i % 7 == 0 { 2 } else { 3 }], "price": 10 * (1 + (i + j) % 6)}))
                .collect();
            let sellers: Vec<Value> = (0..1 + i % 3)
                .map(|j| {
                    let mut seller = json!({"id": ids[(i + j) % 3]});
                    if (i + j) % 4 != 0 {
                        seller["rating"] = json!((i + j) % 5);
                    }
                    seller
                })
                .collect();
            json!({"order": {"lines": lines, "sellers": sellers}})
        })
        .collect();
    columns_match_version_one(&[(PATH, policy)], rows);
}

const TENANT: &str = r#"{
  models: {tenant: {income: "number", limit: "number", label: "string", loans: "loan[]", address: "address"}, loan: {qty: "number", score: "number", plan: "string(basic|plus|premium|trial)"}, address: {balance: "number"}},
  blocks: [
    {expression: "tenant.calc.summary", value: "(split(string(tenant.address.balance <= 2), '-')[1] ?? 'none')"},
    {expression: "tenant.segment", value: "filter(tenant.loans, #.qty in [39..64])"},
    {expression: "loan.flag", value: "{a: tenant.label in ['high', 'low']}"},
    {expression: "loan.adjusted", value: "{s: (loan.flag.a and loan.points), v: `${tenant.calc.summary}/${(tenant.income % (abs(18) + 1))}`, n: loan.plan in ['basic', 'trial']}"},
    {assertion: "band", conditions: [["or", "all(tenant.loans, #.score == #.qty)", 1], ["and", "all(tenant.loans, (#.points and #.points))", 0], ["or", "sum(map(tenant.segment, (#.score * #.qty))) <= 5", 0], ["and", "tenant.address.balance < tenant.income", 1], ["or", "tenant.income in [39..163]", 1]]},
    {assertion: "loan.decision", conditions: [["or", "((band or loan.flag.a) and loan.plan != 'plus')", 1], "sum(map(tenant.segment, -(#.score))) >= 18"]},
    {expression: "loan.meta.review", value: "map(tenant.loans, (((#.meta.fee ?? 0) % (abs((#.meta.fee ?? 0)) + 1)) * (#.decision ? (#.meta.fee ?? 0) : (#.meta.fee ?? 0))))"},
    {expression: "address.eligible", value: "address.balance <= tenant.limit"},
    {table: "t8", inputs: ["tenant.label", "loan.adjusted.v"], outputs: ["loan.meta.fee", "loan.margin[]"], rules: [
      ["", "'high', 'x'", "(max(map(tenant.loans, #.score)) ?? 0)", "tenant.limit"],
      ["", "", "tenant.limit", "loan.qty"],
      ["'ok', 'A1'", "'ok'", "len(tenant.loans)", "-(loan.score)"],
      ["$ != 'gold'", "", "", "loan.score"],
      ["", "'Oslo', 'VIP'", "(loan.points ? tenant.limit : loan.qty)", "loan.score"],
      ["'x'", "startsWith($, 'A1')", "1.5", "tenant.limit"],
    ]},
    {assertion: "loan.points", conditions: [["or", "tenant.label in ['none', 'a']", 1]]},
    {table: "t12", inputs: ["loan.adjusted.n", "loan.adjusted.n", "loan.points"], outputs: ["loan.flags.total", "loan.surcharge"], rules: [
      ["false", "false", "false", "", "loan.adjusted.v"],
      ["", "false", "true", "tenant.income", ""],
      ["", "", "", "tenant.limit", "tenant.risk"],
    ]},
    {match: "tenant.risk", arms: [["(band or band)", "(not (band) ? tenant.label : trim(tenant.calc.summary))"], ["tenant.income == 10", "(band ? tenant.label : tenant.calc.summary)"], ["", "upper(tenant.label)"]]},
  ],
}"#;

fn tenant_rows(address: impl Fn(usize) -> Option<Value>) -> Vec<Value> {
    let labels = ["a-b-c", "O'Neil", "12", "retail", "A1", "ok", "none", "x", "high"];
    let plans = ["basic", "trial", "plus", "premium"];
    (0..54)
        .map(|i| {
            let loans: Vec<Value> = (0..i % 4)
                .map(|j| json!({"qty": ([18, 2, 45, 60, 250])[(i + j) % 5], "score": ([2.0, 45.0, 7.25, 18.0])[(i * 3 + j) % 4], "plan": plans[(i + j) % 4]}))
                .collect();
            let mut tenant = json!({
                "income": ([7.25, -500.0, 2.0, 0.0, 10.0, 100.0])[i % 6],
                "limit": ([-0.5, 7.25, 2.5, 0.0, 250.0])[i % 5],
                "label": labels[i % labels.len()],
                "loans": loans,
            });
            if let Some(address) = address(i) {
                tenant["address"] = address;
            }
            json!({"tenant": tenant})
        })
        .collect()
}

#[test]
fn blocks_on_a_single_relationship_beside_a_list_match_version_one_for_every_goal() {
    let policies = [(PATH, TENANT)];
    let old = workspace(&policies, EngineVersion::V1);
    let new = workspace(&policies, EngineVersion::V2);
    let balances = [3.0, 0.0, -0.5, 18.0, 100.0];
    let present = |i: usize| Some(json!({"balance": balances[i % 5]}));
    let sparse = |i: usize| (i % 3 != 0).then(|| json!({"balance": balances[i % 5]}));
    let nulled = |i: usize| match i % 4 {
        0 => Some(Value::Null),
        1 => Some(json!({"balance": null})),
        _ => Some(json!({"balance": balances[i % 5]})),
    };
    let mut broken = tenant_rows(present);
    broken[4]["tenant"]["limit"] = Value::Null;
    broken[9]["tenant"]["loans"][0]["plan"] = json!("gold");
    broken[11]["tenant"].as_object_mut().expect("tenant").remove("income");
    let goal_sets: [&[&str]; 9] = [
        &[],
        &["loan.flag"],
        &["loan.points"],
        &["address.eligible"],
        &["tenant.segment", "loan.surcharge", "band"],
        &["loan.points", "loan.meta.fee", "loan.flags.total"],
        &["band", "tenant.segment", "address.eligible"],
        &["address.eligible", "loan.adjusted"],
        &["tenant.risk", "loan.meta.review"],
    ];
    for rows in [tenant_rows(present), tenant_rows(sparse), tenant_rows(nulled), tenant_rows(|_| None), broken] {
        let built = Built::nested(&rows)
            .or_else(|| Built::lists(&rows))
            .or_else(|| Built::new(&rows))
            .expect("columns");
        assert_eq!(built.rows, rows.len(), "rows kept by the column builder");
        let fields = built.fields();
        let structs = built.structs(&fields);
        let columns = built.columns_with(&structs);
        for goals in goal_sets {
            let goals: Vec<Arc<str>> = goals.iter().map(|goal| Arc::from(*goal)).collect();
            goals_match(&old, &new, &columns, &goals);
        }
    }
}

#[test]
fn a_list_whose_member_blocks_would_all_be_delegated_shares_rows() {
    let policy = r#"{
      models: {c: {limit: "number", items: "item[]"}, item: {price: "number", kind: "string"}},
      blocks: [
        {table: "t", inputs: [null], outputs: ["item.band"], rules: [["item.price > c.limit", "'high'"], ["", "'low'"]]},
        {expression: "item.double", value: "item.price * 2"},
        {match: "item.label", arms: [["item.kind == 'a'", "'A'"], ["", "'B'"]]},
        {expression: "c.count", value: "len(c.items)"},
      ],
    }"#;
    let rows: Vec<Value> = (0..48)
        .map(|i| {
            let items: Vec<Value> = (0..i % 4)
                .map(|j| json!({"price": 10 * (i + j) % 70, "kind": (["a", "b"])[j % 2]}))
                .collect();
            json!({"c": {"limit": 30, "items": items}})
        })
        .collect();
    let paths = columns_match_version_one(&[(PATH, policy)], rows);
    assert_eq!(paths.typed, 0, "{paths:?}");
}

const REPORTED: &str = r#"{
  models: {customer: {tier: "string", limit: "number", report: "creditReport", contact: "contact?"}, creditReport: {score: "number", bankrupt: "boolean", opened: "date?"}, contact: {email: "string?", phone: "string?"}},
  blocks: [
    {match: "creditReport.grade", arms: [["creditReport.bankrupt", "'X'"], ["creditReport.score >= 700", "'A'"], ["creditReport.score >= customer.limit", "upper(customer.tier)"], ["", "'B'"]]},
    {expression: "creditReport.double", value: "creditReport.score * 2 + customer.report.score"},
    {assertion: "creditReport.ok", conditions: ["creditReport.double > 100", "not creditReport.bankrupt"]},
    {table: "t", inputs: ["creditReport.score", "customer.tier"], outputs: ["creditReport.band", "creditReport.fee"], rules: [
      ["> 700", "'gold'", "'top'", "creditReport.double / 10"],
      ["> 100", "", "'mid'", "5"],
      ["", "", "'low'", "customer.limit"],
    ]},
    {expression: "creditReport.age", value: "creditReport.opened != null ? year(creditReport.opened) : 0"},
    {expression: "contact.reach", value: "contact.email ?? contact.phone ?? 'none'"},
    {match: "contact.kind", arms: [["contact.email != null", "'mail'"], ["", "'other'"]]},
    {match: "customer.decision", arms: [["customer.report.grade == 'A' and customer.report.ok", "'approve'"], ["customer.report.grade == 'X'", "'decline'"], ["customer.contact.reach == 'none'", "'call'"], ["", "'refer'"]]},
    {expression: "customer.note", value: "`${customer.report.band}/${customer.contact.kind ?? '-'}`"},
  ],
}"#;

fn reported_rows(report: impl Fn(usize) -> Option<Value>, contact: impl Fn(usize) -> Option<Value>) -> Vec<Value> {
    let tiers = ["gold", "silver", "basic"];
    (0..48)
        .map(|i| {
            let mut customer = json!({"tier": tiers[i % 3], "limit": ([650, 700, 20, 900])[i % 4]});
            if let Some(report) = report(i) {
                customer["report"] = report;
            }
            if let Some(contact) = contact(i) {
                customer["contact"] = contact;
            }
            json!({"customer": customer})
        })
        .collect()
}

fn full_report(i: usize) -> Value {
    json!({"score": ([800, 650, 10, 705, 30])[i % 5], "bankrupt": i % 7 == 0, "opened": (["2020-01-05", "2024-12-31", "1999-06-30"])[i % 3]})
}

fn full_contact(i: usize) -> Value {
    match i % 3 {
        0 => json!({"email": "a@b.c", "phone": "1"}),
        1 => json!({"phone": "2"}),
        _ => json!({"email": "x@y.z"}),
    }
}

fn each_goal(policy: &str, rows: &[Value], goal_sets: &[&[&str]]) -> Vec<Paths> {
    let policies = [(PATH, policy)];
    let old = workspace(&policies, EngineVersion::V1);
    let new = workspace(&policies, EngineVersion::V2);
    let built = Built::nested(rows)
        .or_else(|| Built::lists(rows))
        .or_else(|| Built::new(rows))
        .expect("columns");
    assert_eq!(built.rows, rows.len(), "rows kept by the column builder");
    let fields = built.fields();
    let structs = built.structs(&fields);
    let columns = built.columns_with(&structs);
    goal_sets
        .iter()
        .map(|goals| {
            let goals: Vec<Arc<str>> = goals.iter().map(|goal| Arc::from(*goal)).collect();
            goals_match(&old, &new, &columns, &goals)
        })
        .collect()
}

const REPORTED_GOALS: [&[&str]; 8] = [
    &[],
    &["creditReport.grade"],
    &["creditReport.fee"],
    &["contact.reach"],
    &["customer.decision"],
    &["customer.note", "creditReport.ok"],
    &["contact.kind", "creditReport.age"],
    &["creditReport.band", "customer.decision", "contact.reach"],
];

#[test]
fn blocks_on_single_relationships_run_typed_without_row_objects() {
    let rows = reported_rows(|i| Some(full_report(i)), |i| Some(full_contact(i)));
    let paths = columns_match_version_one(&[(PATH, REPORTED)], rows.clone());
    assert!(paths.typed > 0 && paths.hosted == 0, "{paths:?}");
    each_goal(REPORTED, &rows, &REPORTED_GOALS);
    let plain = r#"{
      models: {customer: {limit: "number", report: "creditReport"}, creditReport: {score: "number", bankrupt: "boolean"}},
      blocks: [
        {match: "creditReport.grade", arms: [["creditReport.bankrupt", "'X'"], ["creditReport.score >= customer.limit", "'A'"], ["", "'B'"]]},
        {expression: "creditReport.double", value: "creditReport.score * 2"},
        {table: "t", inputs: ["creditReport.score"], outputs: ["creditReport.band"], rules: [["> 700", "'top'"], ["", "'low'"]]},
        {expression: "customer.sum", value: "customer.report.double + customer.limit"},
      ],
    }"#;
    typed_like_version_one(&[(PATH, plain)], rows);
}

#[test]
fn single_relationships_that_are_missing_null_or_empty_match_version_one_for_every_goal() {
    let partial = |i: usize| match i % 6 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(json!({"score": 720})),
        3 => Some(json!({"bankrupt": false})),
        4 => Some(json!({"score": null, "bankrupt": null})),
        _ => Some(full_report(i)),
    };
    let sparse = |i: usize| match i % 4 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(json!({"email": null, "phone": null})),
        _ => Some(full_contact(i)),
    };
    let sets = [
        reported_rows(|i| Some(full_report(i)), sparse),
        reported_rows(|i| Some(full_report(i)), |_| None),
        reported_rows(|i| Some(full_report(i)), |_| Some(Value::Null)),
        reported_rows(partial, |i| Some(full_contact(i))),
        reported_rows(partial, sparse),
        reported_rows(|_| None, |_| None),
        reported_rows(|i| (i % 2 == 0).then(|| full_report(i)), |i| (i % 3 == 0).then(|| full_contact(i))),
    ];
    for rows in &sets {
        each_goal(REPORTED, rows, &REPORTED_GOALS);
    }
}

#[test]
fn single_relationships_holding_what_the_model_refuses_match_version_one() {
    let mut rows = reported_rows(|i| Some(full_report(i)), |i| Some(full_contact(i)));
    rows[1]["customer"]["report"]["score"] = json!("high");
    rows[2]["customer"]["report"]["bankrupt"] = json!(3);
    rows[5]["customer"]["report"]["opened"] = json!("not a date");
    rows[7]["customer"]["contact"]["email"] = json!(false);
    rows[9]["customer"]["limit"] = Value::Null;
    rows[11]["customer"]["report"]["score"] = Value::Null;
    rows[13]["customer"].as_object_mut().expect("customer").remove("tier");
    each_goal(REPORTED, &rows, &REPORTED_GOALS);
}

#[test]
fn a_relationship_given_under_its_entity_name_does_not_stand_in_for_the_related_object() {
    let mut rows = reported_rows(|i| (i % 2 == 0).then(|| full_report(i)), |i| Some(full_contact(i)));
    for (i, row) in rows.iter_mut().enumerate() {
        if i % 4 == 1 {
            row["creditReport"] = full_report(i);
        }
        if i % 8 == 3 {
            row["contact"] = full_contact(i);
        }
    }
    each_goal(REPORTED, &rows, &REPORTED_GOALS);
}

const CONFIGURED: &str = r#"{
  globals: {env: {cfg: "config", cap: "number", perk: "extra?"}},
  models: {config: {base: "number", mode: "string(lin|sq)"}, extra: {bonus: "number?"}, o: {items: "item[]"}, item: {v: "number"}},
  blocks: [
    {match: "config.factor", arms: [["config.mode == 'sq'", "config.base * config.base"], ["", "config.base"]]},
    {expression: "config.double", value: "config.base * 2 + cap"},
    {expression: "extra.total", value: "(extra.bonus ?? 1) + cfg.double"},
    {match: "item.tier", arms: [["item.v > cfg.double", "'hi'"], ["item.v > (perk.total ?? 0)", "'mid'"], ["", "'lo'"]]},
    {expression: "factor", value: "cfg.factor + 1 + (perk.total ?? 0)"},
    {expression: "o.count", value: "len(filter(o.items, #.tier == 'hi'))"},
  ],
}"#;

fn configured_rows(cfg: impl Fn(usize) -> Option<Value>, extra: impl Fn(usize) -> Option<Value>) -> Vec<Value> {
    (0..40)
        .map(|i| {
            let items: Vec<Value> = (0..i % 4).map(|j| json!({"v": (i * 3 + j * 5) % 23})).collect();
            let mut row = json!({"cap": i % 5, "o": {"items": items}});
            if let Some(cfg) = cfg(i) {
                row["cfg"] = cfg;
            }
            if let Some(extra) = extra(i) {
                row["perk"] = extra;
            }
            row
        })
        .collect()
}

const CONFIGURED_GOALS: [&[&str]; 6] = [
    &[],
    &["config.factor"],
    &["factor"],
    &["item.tier"],
    &["extra.total", "o.count"],
    &["config.double"],
];

#[test]
fn blocks_on_global_single_relationships_run_typed() {
    let cfg = |i: usize| Some(json!({"base": (i % 7) as i64 - 2, "mode": (["lin", "sq"])[i % 2]}));
    let rows = configured_rows(cfg, |i| Some(json!({"bonus": i % 3})));
    typed_like_version_one(&[(PATH, CONFIGURED)], rows.clone());
    each_goal(CONFIGURED, &rows, &CONFIGURED_GOALS);
}

#[test]
fn global_single_relationships_that_are_missing_null_or_partial_match_version_one() {
    let cfg = |i: usize| match i % 5 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(json!({"base": 3})),
        3 => Some(json!({"mode": "sq"})),
        _ => Some(json!({"base": (i % 7) as i64 - 2, "mode": (["lin", "sq"])[i % 2]})),
    };
    let full = |i: usize| Some(json!({"base": (i % 7) as i64 - 2, "mode": (["lin", "sq"])[i % 2]}));
    let extra = |i: usize| match i % 4 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(json!({"bonus": null})),
        _ => Some(json!({"bonus": i % 3})),
    };
    for rows in [configured_rows(cfg, extra), configured_rows(full, extra), configured_rows(cfg, |_| None), configured_rows(full, |_| None)] {
        each_goal(CONFIGURED, &rows, &CONFIGURED_GOALS);
    }
}

#[test]
fn single_relationship_blocks_the_columns_cannot_alias_match_version_one() {
    let owner = r#"{
      models: {customer: {tier: "string", limit: "number", report: "creditReport"}, creditReport: {score: "number", bankrupt: "boolean"}},
      blocks: [
        {expression: "creditReport.over", value: "creditReport.score > creditReport.customer.limit"},
        {expression: "creditReport.keys", value: "len(keys(creditReport))"},
        {expression: "customer.flag", value: "customer.report.over and customer.report.keys > 1"},
      ],
    }"#;
    let shadow = r#"{
      models: {customer: {tier: "string", limit: "number", scores: "number[]", report: "creditReport"}, creditReport: {score: "number", bankrupt: "boolean"}},
      blocks: [
        {expression: "creditReport.above", value: "len(filter(customer.scores as creditReport, creditReport > 5)) + creditReport.score"},
        {expression: "customer.flag", value: "customer.report.above > 3"},
      ],
    }"#;
    let rows: Vec<Value> = reported_rows(|i| (i % 5 != 0).then(|| json!({"score": i % 9, "bankrupt": i % 2 == 0})), |_| None)
        .into_iter()
        .enumerate()
        .map(|(i, mut row)| {
            row["customer"]["scores"] = json!([i % 4, 7, i % 11]);
            row
        })
        .collect();
    let table = r#"{
      models: {customer: {tier: "string", limit: "number", report: "creditReport"}, creditReport: {score: "number", bankrupt: "boolean"}},
      blocks: [
        {table: "t", inputs: ["creditReport.score"], outputs: ["creditReport.band"], rules: [["> customer.limit", "'over'"], ["", "'under'"]]},
        {expression: "creditReport.twice", value: "creditReport.score * 2"},
        {expression: "customer.flag", value: "customer.report.band == 'over' and customer.report.twice > 4"},
      ],
    }"#;
    columns_match_version_one(&[(PATH, owner)], rows.clone());
    columns_match_version_one(&[(PATH, shadow)], rows.clone());
    let paths = columns_match_version_one(&[(PATH, table)], rows);
    assert!(paths.typed > 0, "{paths:?}");
}

const CARTED: &str = r#"{
  globals: {env: {cap: "number"}},
  models: {cart: {items: "&product[]", note: "string?"}, product: {id: "string", price: "number", kind: "string?"}},
  blocks: [
    {table: "dt", inputs: ["product.price"], outputs: ["product.band"], rules: [["> 3", "'hi'"], ["", "'lo'"]]},
    {match: "product.capped", arms: [["product.price > cap", "cap"], ["", "product.price"]]},
    {expression: "product.double", value: "product.price * 2"},
    {expression: "cart.ids", value: "map(cart.items, #.id)"},
    {expression: "cart.total", value: "sum(map(cart.items as c, c.price))"},
    {expression: "cart.n", value: "len(cart.items)"},
    {expression: "cart.as", value: "count(cart.items, #.kind == 'a')"},
    {expression: "cart.doubled", value: "sum(map(cart.items, #.price * 2)) + cap"},
  ],
}"#;

fn carted_rows(count: usize, items: impl Fn(usize) -> Option<Value>, pool: impl Fn(usize) -> Option<Value>) -> Vec<Value> {
    (0..count)
        .map(|i| {
            let mut cart = json!({});
            if i % 3 == 0 {
                cart["note"] = json!("n");
            }
            if let Some(items) = items(i) {
                cart["items"] = items;
            }
            let mut row = json!({"cap": (i % 4) + 1, "cart": cart});
            if let Some(pool) = pool(i) {
                row["product"] = pool;
            }
            row
        })
        .collect()
}

fn product_pool(i: usize) -> Value {
    json!([
        {"id": "p1", "price": 5 + (i % 3), "kind": "a"},
        {"id": "p2", "price": 1},
        {"id": "p3", "price": (i % 7) as f64 + 0.5, "kind": "b"},
    ])
}

fn product_keys(i: usize) -> Value {
    match i % 6 {
        0 => json!([]),
        1 => json!(["p1"]),
        2 => json!(["p2", "p1", "p1"]),
        3 => json!(["p3", "p2"]),
        4 => json!(["p3"]),
        _ => json!(["p1", "p2", "p3", "p2"]),
    }
}

const CARTED_GOALS: [&[&str]; 6] = [
    &[],
    &["product.band"],
    &["cart.ids"],
    &["cart.total", "cart.as"],
    &["product.capped", "cart.n"],
    &["cart.doubled"],
];

#[test]
fn lists_of_references_to_a_pool_with_member_blocks_run_by_columns() {
    let rows = carted_rows(48, |i| Some(product_keys(i)), |i| Some(product_pool(i)));
    let paths = columns_match_version_one(&[(PATH, CARTED)], rows.clone());
    assert!(paths.typed > 0 && paths.hosted == 0, "{paths:?}");
    each_goal(CARTED, &rows, &CARTED_GOALS);
}

#[test]
fn lists_of_references_that_are_missing_null_unknown_or_repeated_match_version_one() {
    let odd_keys = |i: usize| match i % 9 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(json!(["zz"])),
        3 => Some(json!(["p1", "zz", "p2"])),
        4 => Some(json!(["p1", null, "p2"])),
        5 => Some(json!([3])),
        6 => Some(json!(["p2", "p2", "p2", "p2"])),
        _ => Some(product_keys(i)),
    };
    let odd_pool = |i: usize| match i % 8 {
        0 => None,
        1 => Some(Value::Null),
        2 => Some(json!([])),
        3 => Some(json!([{"id": "p1", "price": 2}, {"id": "p1", "price": 9}, {"id": "p2", "price": 4}, {"id": "p3", "price": 1}])),
        4 => Some(json!([{"price": 2}, {"id": "p1", "price": 9}, {"id": "p2", "price": 4}, {"id": "p3", "price": 1}])),
        5 => Some(json!([{"id": "p1"}, {"id": "p2", "price": 4}, {"id": "p3", "price": 1}])),
        _ => Some(product_pool(i)),
    };
    let sets = [
        carted_rows(72, odd_keys, |i| Some(product_pool(i))),
        carted_rows(72, |i| Some(product_keys(i)), odd_pool),
        carted_rows(72, odd_keys, odd_pool),
        carted_rows(24, |_| None, |_| None),
        carted_rows(24, |_| Some(json!([])), |_| Some(json!([]))),
        carted_rows(24, |i| Some(product_keys(i)), |_| None),
    ];
    for rows in &sets {
        each_goal(CARTED, rows, &CARTED_GOALS);
    }
    let mut broken = carted_rows(48, |i| Some(product_keys(i)), |i| Some(product_pool(i)));
    broken[3].as_object_mut().expect("row").remove("cap");
    broken[5]["product"][0]["price"] = json!("dear");
    broken[7]["product"][1] = Value::Null;
    broken[9]["cap"] = Value::Null;
    each_goal(CARTED, &broken, &CARTED_GOALS);
}

#[test]
fn lists_of_references_in_other_shapes_match_version_one() {
    let unblocked = r#"{
      models: {cart: {items: "&product[]"}, product: {id: "string", price: "number", kind: "string?"}},
      blocks: [
        {expression: "cart.total", value: "sum(map(cart.items, #.price))"},
        {expression: "cart.kinds", value: "map(cart.items, #.kind ?? '-')"},
        {expression: "cart.first", value: "len(cart.items) > 0 ? cart.items[0].id : 'none'"},
        {expression: "cart.pooled", value: "len(product)"},
      ],
    }"#;
    let single = r#"{
      models: {order: {zone: "&zone", lines: "line[]"}, line: {qty: "number"}, zone: {id: "string", shipping: "number"}},
      blocks: [
        {expression: "zone.fee", value: "zone.shipping * 2"},
        {expression: "line.double", value: "line.qty * 2"},
        {expression: "order.total", value: "order.zone.fee + sum(map(order.lines, #.double))"},
      ],
    }"#;
    let computed = r#"{
      models: {cart: {items: "&product[]"}, product: {id: "string", price: "number"}},
      blocks: [
        {expression: "product.double", value: "product.price * 2"},
        {match: "product.band", arms: [["product.price > 3", "'hi'"], ["", "'lo'"]]},
        {expression: "cart.his", value: "count(cart.items, #.band == 'hi')"},
        {expression: "cart.doubled", value: "sum(map(cart.items, #.double))"},
      ],
    }"#;
    let whole = r#"{
      models: {cart: {items: "&product[]"}, product: {id: "string", price: "number"}},
      blocks: [
        {expression: "product.double", value: "product.price * 2"},
        {expression: "cart.size", value: "len(keys(cart))"},
        {expression: "cart.same", value: "map(cart.items, #.double == product[0].double)"},
      ],
    }"#;
    let rows = carted_rows(48, |i| Some(product_keys(i)), |i| Some(product_pool(i)));
    let paths = columns_match_version_one(&[(PATH, unblocked)], rows.clone());
    assert_eq!(paths.hosted, 0, "{paths:?}");
    columns_match_version_one(&[(PATH, whole)], rows.clone());
    columns_match_version_one(&[(PATH, computed)], rows);
    let orders: Vec<Value> = (0..40)
        .map(|i| {
            let lines: Vec<Value> = (0..i % 3).map(|j| json!({"qty": i + j})).collect();
            json!({
                "order": {"zone": (["eu", "us", "eu"])[i % 3], "lines": lines},
                "zone": [{"id": "eu", "shipping": 4.5}, {"id": "us", "shipping": 12}],
            })
        })
        .collect();
    columns_match_version_one(&[(PATH, single)], orders);
}

const DEMANDED_CELLS: &str = r#"{
  models: {household: {income: "number", size: "number", assets: "number?", members: "member[]"}, member: {age: "number", earns: "number"}},
  blocks: [
    {expression: "household.need", value: "household.size * 400"},
    {expression: "household.gap", value: "household.need - household.income"},
    {expression: "household.spare", value: "household.income / (household.size - 3)"},
    {expression: "member.share", value: "member.earns * 2"},
    {table: "benefit", inputs: ["household.assets", "household.gap"], outputs: ["household.benefit", "household.tier"], rules: [
      ["> 50000", "", "0", "'ineligible'"],
      ["", "<= 0", "0", "'not-needed'"],
      ["", "< 200", "200", "'minimum'"],
      ["", "", "round(household.gap, 0)", "'top-up'"],
    ]},
    {table: "extra", inputs: ["household.gap"], outputs: ["household.extra"], rules: [
      ["> 900", "household.spare"],
      ["", "household.gap / 2"],
    ]},
    {match: "household.band", arms: [
      ["household.gap > 500", "household.gap * 2"],
      ["household.gap > 0", "household.need"],
      ["", "0"],
    ]},
    {table: "memberBand", inputs: ["member.share", "member.age"], outputs: ["member.band"], rules: [
      ["> 1000", "", "member.share - 1000"],
      ["", "< 18", "0"],
      ["", "", "member.share"],
    ]},
  ],
}"#;

fn demanded_rows() -> Vec<Value> {
    (0..80)
        .map(|i| {
            let members: Vec<Value> = (0..i % 3).map(|j| json!({"age": 10 + (i * 7 + j * 11) % 60, "earns": (i * 97 + j * 31) % 900})).collect();
            let mut household = json!({"income": (i * 131) % 2600, "size": 1 + i % 6, "members": members});
            match i % 5 {
                0 => {}
                1 => household["assets"] = Value::Null,
                _ => household["assets"] = json!((i * 1777) % 90000),
            }
            json!({"household": household})
        })
        .collect()
}

#[test]
fn table_and_match_cells_reading_fields_the_block_already_read_run_typed() {
    let policy = r#"{
      models: {household: {income: "number", size: "number", assets: "number?"}},
      blocks: [
        {expression: "household.need", value: "household.size * 400"},
        {expression: "household.gap", value: "household.need - household.income"},
        {table: "benefit", inputs: ["household.assets", "household.gap"], outputs: ["household.benefit", "household.tier"], rules: [
          ["> 50000", "", "0", "'ineligible'"],
          ["", "<= 0", "0", "'not-needed'"],
          ["", "< 200", "200", "'minimum'"],
          ["", "", "round(household.gap, 0)", "'top-up'"],
        ]},
        {match: "household.band", arms: [
          ["household.gap > 500", "household.gap * 2"],
          ["", "0"],
        ]},
      ],
    }"#;
    let rows: Vec<Value> = demanded_rows()
        .into_iter()
        .enumerate()
        .map(|(i, mut row)| {
            let household = row["household"].as_object_mut().expect("household");
            household.remove("members");
            household.insert("assets".into(), json!((i * 1777) % 90000));
            row
        })
        .collect();
    typed_like_version_one(&[(PATH, policy)], rows);
}

#[test]
fn cells_reading_already_demanded_and_other_computed_fields_match_version_one() {
    let rows = demanded_rows();
    columns_match_version_one(&[(PATH, DEMANDED_CELLS)], rows.clone());
    each_goal(
        DEMANDED_CELLS,
        &rows,
        &[&[], &["household.benefit"], &["household.extra"], &["household.band", "member.band"], &["household.tier"]],
    );
    let mut broken = rows;
    broken[3]["household"]["income"] = Value::Null;
    broken[4]["household"]["size"] = json!(3);
    broken[9]["household"]["size"] = json!(3);
    broken[7]["household"].as_object_mut().expect("household").remove("size");
    columns_match_version_one(&[(PATH, DEMANDED_CELLS)], broken);
}

const GATHERED: &str = r#"{
  models: {
    account: {structuring: "boolean", volume: "number", corridors: "number?", kind: "string", items: "item[]"},
    item: {price: "number", qty: "number", tag: "string?"},
  },
  blocks: [
    {expression: "account.ratio", value: "account.volume / 100"},
    {table: "alerts", hit: "collect", inputs: ["account.structuring", "account.ratio", "account.corridors"], outputs: ["account.alerts"], rules: [
      ["true", "", "", "'structuring'"],
      ["", "> 3", "", "'volume-spike'"],
      ["", "", "> 0", "'high-risk-corridor'"],
    ]},
    {table: "notes", inputs: ["account.kind", "account.ratio"], outputs: ["account.notes[]", "account.codes[]"], rules: [
      ["'retail'", "", "'retail'", "1"],
      ["", "> 5", "account.ratio * 2", ""],
      ["'retail', 'sme'", "< 2", "", "account.volume % 7"],
      ["", "", "'seen'", "{code: 9}"],
    ]},
    {table: "both", inputs: ["account.kind"], outputs: ["account.marks[]", "account.marks[]"], rules: [
      ["'sme'", "'a'", "'b'"],
      ["", "", "'c'"],
    ]},
    {expression: "account.decision", value: "'structuring' in account.alerts ? 'file' : len(account.alerts) > 0 ? 'look' : 'none'"},
    {expression: "item.total", value: "item.price * item.qty"},
    {table: "flags", inputs: ["item.total", "item.tag"], outputs: ["item.flags[]"], rules: [
      ["> 500", "", "'big'"],
      ["", "'gift'", "'gift'"],
      ["< 10", "", "'small'"],
    ]},
    {expression: "account.flagged", value: "count(account.items, len(#.flags) > 0)"},
  ],
}"#;

fn gathered_rows() -> Vec<Value> {
    let kinds = ["retail", "sme", "corp"];
    (0..90)
        .map(|i| {
            let items: Vec<Value> = (0..i % 4)
                .map(|j| {
                    let mut item = json!({"price": (i * 13 + j * 29) % 300, "qty": 1 + (i + j) % 4});
                    if (i + j) % 3 == 0 {
                        item["tag"] = json!((["gift", "plain"])[(i + j) % 2]);
                    }
                    item
                })
                .collect();
            let mut account = json!({"structuring": i % 7 == 0, "volume": (i * 53) % 900, "kind": kinds[i % 3], "items": items});
            match i % 8 {
                0 => {}
                1 => account["corridors"] = Value::Null,
                _ => account["corridors"] = json!(i % 3),
            }
            json!({"account": account})
        })
        .collect()
}

#[test]
fn collect_tables_run_typed() {
    let policy = r#"{
      models: {account: {structuring: "boolean", volume: "number", corridors: "number"}},
      blocks: [
        {expression: "account.ratio", value: "account.volume / 100"},
        {table: "alerts", hit: "collect", inputs: ["account.structuring", "account.ratio", "account.corridors"], outputs: ["account.alerts"], rules: [
          ["true", "", "", "'structuring'"],
          ["", "> 3", "", "'volume-spike'"],
          ["", "", "> 0", "'high-risk-corridor'"],
          ["false", "> 8", "", "account.ratio"],
        ]},
        {expression: "account.decision", value: "'structuring' in account.alerts ? 'file' : len(account.alerts) > 0 ? 'look' : 'none'"},
      ],
    }"#;
    let rows: Vec<Value> = (0..90)
        .map(|i| json!({"account": {"structuring": i % 7 == 0, "volume": (i * 53) % 900, "corridors": i % 3}}))
        .collect();
    typed_like_version_one(&[(PATH, policy)], rows);
}

#[test]
fn collect_tables_with_several_outputs_members_and_failures_match_version_one() {
    let rows = gathered_rows();
    columns_match_version_one(&[(PATH, GATHERED)], rows.clone());
    each_goal(
        GATHERED,
        &rows,
        &[&[], &["account.alerts"], &["account.decision"], &["account.flagged"], &["account.notes", "account.marks"]],
    );
    let mut broken = rows;
    broken[2]["account"]["volume"] = Value::Null;
    broken[5]["account"]["kind"] = json!(4);
    broken[9]["account"]["items"][0]["price"] = json!("x");
    broken[11]["account"].as_object_mut().expect("account").remove("structuring");
    broken[13]["account"]["corridors"] = json!("many");
    columns_match_version_one(&[(PATH, GATHERED)], broken);
}

#[test]
fn collect_cells_that_fail_only_on_unmatched_rules_match_version_one() {
    let policy = r#"{
      models: {order: {qty: "number", price: "number", code: "string?"}},
      blocks: [
        {table: "notes", hit: "collect", inputs: ["order.qty"], outputs: ["order.notes"], rules: [
          ["> 0", "order.price / order.qty"],
          ["0", "'empty'"],
          ["< 0", "len(order.code)"],
          ["", "order.qty"],
        ]},
      ],
    }"#;
    let rows: Vec<Value> = (0..64)
        .map(|i| {
            let mut order = json!({"qty": (i % 5) as i64 - 2, "price": 10 * i});
            if i % 3 == 0 {
                order["code"] = json!("abc");
            }
            json!({"order": order})
        })
        .collect();
    columns_match_version_one(&[(PATH, policy)], rows);
}

const SPARSE_FIELDS: &str = r#"{
  models: {
    employer: {bonus: "number?", region: "string?", audited: "boolean?", employees: "employee[]"},
    employee: {salary: "number", smoker: "boolean?", grade: "string?", extra: "number?"},
  },
  blocks: [
    {expression: "employee.rate", value: "(employee.smoker ?? false) ? 1.5 : 1"},
    {expression: "employee.label", value: "(employee.grade ?? 'none') + '-' + string(employee.salary)"},
    {expression: "employee.pay", value: "employee.salary * employee.rate + (employee.extra ?? 0)"},
    {expression: "employer.total", value: "sum(map(employer.employees, #.pay)) + (employer.bonus ?? 0)"},
    {expression: "employer.where", value: "employer.region == null ? 'unknown' : employer.region"},
    {expression: "employer.checked", value: "employer.audited == true"},
  ],
}"#;

fn optional_rows() -> Vec<Value> {
    (0..120)
        .map(|i| {
            let employees: Vec<Value> = (0..i % 4)
                .map(|j| {
                    let mut e = json!({"salary": 1000 + (i * 17 + j * 5) % 300});
                    match (i + j) % 4 {
                        0 => {}
                        1 => e["smoker"] = Value::Null,
                        2 => e["smoker"] = json!(true),
                        _ => e["smoker"] = json!(false),
                    }
                    match (i + 2 * j) % 3 {
                        0 => {}
                        1 => e["grade"] = Value::Null,
                        _ => e["grade"] = json!((["a", "b"])[j % 2]),
                    }
                    match (i + j) % 5 {
                        0 => e["extra"] = Value::Null,
                        1 | 2 => e["extra"] = json!((i * j) % 9),
                        _ => {}
                    }
                    e
                })
                .collect();
            let mut employer = json!({"employees": employees});
            match i % 3 {
                0 => {}
                1 => {
                    employer["bonus"] = Value::Null;
                    employer["region"] = Value::Null;
                    employer["audited"] = Value::Null;
                }
                _ => {
                    employer["bonus"] = json!(i % 11);
                    employer["region"] = json!((["north", "south"])[i % 2]);
                    employer["audited"] = json!(i % 2 == 0);
                }
            }
            json!({"employer": employer})
        })
        .collect()
}

#[test]
fn optional_fields_given_as_null_or_left_out_run_typed() {
    typed_like_version_one(&[(PATH, SPARSE_FIELDS)], optional_rows());
}

#[test]
fn optional_fields_given_as_null_left_out_or_wrongly_typed_match_version_one() {
    let mut rows = optional_rows();
    rows[5]["employer"]["employees"][0]["smoker"] = json!("yes");
    rows[6]["employer"]["bonus"] = json!("big");
    rows[9]["employer"]["employees"][0]["extra"] = json!(true);
    columns_match_version_one(&[(PATH, SPARSE_FIELDS)], rows.clone());
    each_goal(SPARSE_FIELDS, &rows, &[&[], &["employee.rate"], &["employer.where"], &["employer.total"]]);
}

const TRANSITIVE_CELLS: &str = r#"{
  models: {
    invoice: {amount: "number", paid: "number", vip: "boolean", lines: "line[]"},
    line: {qty: "number", price: "number"},
  },
  blocks: [
    {expression: "invoice.outstanding", value: "invoice.amount - invoice.paid"},
    {expression: "invoice.overdue", value: "invoice.outstanding > 100"},
    {match: "invoice.stage", arms: [
      ["invoice.outstanding <= 0", "'paid'"],
      ["invoice.overdue and invoice.vip", "'soft'"],
      ["invoice.overdue", "'hard'"],
      ["", "'open'"],
    ]},
    {table: "fees", inputs: ["invoice.stage"], outputs: ["invoice.fee"], rules: [
      ["'hard'", "round(invoice.outstanding * 0.1, 2)"],
      ["'soft'", "invoice.outstanding / (invoice.paid - 50)"],
      ["", "0"],
    ]},
    {assertion: "invoice.ok", conditions: ["invoice.fee < 1000", "invoice.outstanding < 5000"]},
    {match: "invoice.payment", arms: [
      ["invoice.ok", "invoice.fee + invoice.outstanding"],
      ["", "0"],
    ]},
    {expression: "line.total", value: "line.qty * line.price"},
    {expression: "line.big", value: "line.total > 50"},
    {match: "line.charge", arms: [
      ["line.big", "line.total * 0.9"],
      ["", "line.total"],
    ]},
  ],
}"#;

fn transitive_rows() -> Vec<Value> {
    (0..96)
        .map(|i| {
            let lines: Vec<Value> = (0..i % 3).map(|j| json!({"qty": 1 + (i + j) % 4, "price": (i * 7 + j * 13) % 40})).collect();
            json!({"invoice": {"amount": (i * 97) % 3000, "paid": (i * 31) % 700, "vip": i % 5 == 0, "lines": lines}})
        })
        .collect()
}

#[test]
fn cells_reading_fields_demanded_through_other_blocks_run_typed() {
    let rows: Vec<Value> = transitive_rows()
        .into_iter()
        .enumerate()
        .map(|(i, mut row)| {
            row["invoice"]["paid"] = json!(100 + (i * 31) % 600);
            row
        })
        .collect();
    typed_like_version_one(&[(PATH, TRANSITIVE_CELLS)], rows);
}

#[test]
fn cells_reading_fields_demanded_through_other_blocks_match_version_one() {
    let rows = transitive_rows();
    columns_match_version_one(&[(PATH, TRANSITIVE_CELLS)], rows.clone());
    each_goal(
        TRANSITIVE_CELLS,
        &rows,
        &[&[], &["invoice.fee"], &["invoice.payment"], &["line.charge"], &["invoice.stage", "invoice.ok"]],
    );
}

const MIXED_GATHER: &str = r#"{
  models: {customer: {age: "number", pep: "boolean", docs: "boolean", country: "string", score: "number?", accounts: "account[]"}, account: {balance: "number", kind: "string"}},
  blocks: [
    {match: "customer.risk", arms: [["customer.pep", "'high'"], ["customer.country in ['NO', 'SE']", "'low'"], ["", "'medium'"]]},
    {table: "outcome", inputs: ["customer.docs", "customer.risk"], outputs: ["customer.outcome", "customer.reasons[]", "customer.level"], rules: [
      ["false", "", "'reject'", "'identity'", ""],
      ["", "'high'", "'refer'", "'pep'", "3"],
      ["", "'medium'", "", "'review'", "customer.age / 10"],
      ["", "", "'accept'", "", "1"],
    ]},
    {table: "points", inputs: ["customer.score"], outputs: ["customer.points[]", "customer.first"], rules: [
      ["> 700", "customer.score - 700", "'top'"],
      ["> 500", "10", ""],
      ["", "0", "customer.score * 2"],
    ]},
    {table: "accountFlags", inputs: ["account.balance", "account.kind"], outputs: ["account.flags[]", "account.tier"], rules: [
      ["< 0", "", "'overdrawn'", "'watch'"],
      ["", "'savings'", "'saver'", ""],
      ["> 10000", "", "'large'", "'gold'"],
    ]},
    {expression: "customer.summary", value: "customer.outcome + ':' + string(len(customer.reasons))"},
    {expression: "account.label", value: "string(account.tier ?? 0) + '/' + string(account.balance)"},
    {expression: "customer.levelText", value: "string(customer.level ?? 0)"},
  ],
}"#;

fn mixed_rows() -> Vec<Value> {
    (0..90)
        .map(|i| {
            let accounts: Vec<Value> = (0..i % 3)
                .map(|j| json!({"balance": (i as i64 * 977 + j as i64 * 131) % 20000 - 3000, "kind": (["savings", "current"])[(i + j) % 2]}))
                .collect();
            let mut customer = json!({"age": 18 + i % 60, "pep": i % 9 == 0, "docs": i % 11 != 0, "country": (["NO", "DE", "SE", "FR"])[i % 4], "accounts": accounts});
            if i % 6 != 0 {
                customer["score"] = json!(300 + (i * 37) % 600);
            }
            json!({"customer": customer})
        })
        .collect()
}

#[test]
fn collect_tables_with_first_hit_outputs_match_version_one() {
    let rows = mixed_rows();
    columns_match_version_one(&[(PATH, MIXED_GATHER)], rows.clone());
    each_goal(
        MIXED_GATHER,
        &rows,
        &[&[], &["customer.outcome"], &["customer.level", "account.tier"], &["customer.summary"], &["customer.first"], &["account.label", "customer.levelText"]],
    );
}

#[test]
fn collect_tables_with_first_hit_outputs_run_typed() {
    let policy = r#"{
      models: {customer: {pep: "boolean", docs: "boolean", country: "string"}},
      blocks: [
        {match: "customer.risk", arms: [["customer.pep", "'high'"], ["customer.country in ['NO', 'SE']", "'low'"], ["", "'medium'"]]},
        {table: "outcome", inputs: ["customer.docs", "customer.risk"], outputs: ["customer.outcome", "customer.reasons[]"], rules: [
          ["false", "", "'reject'", "'identity'"],
          ["", "'high'", "'refer'", "'pep'"],
          ["", "", "'accept'", ""],
        ]},
        {expression: "customer.summary", value: "customer.outcome + ':' + string(len(customer.reasons))"},
      ],
    }"#;
    let rows: Vec<Value> = (0..90)
        .map(|i| json!({"customer": {"pep": i % 9 == 0, "docs": i % 11 != 0, "country": (["NO", "DE", "SE", "FR"])[i % 4]}}))
        .collect();
    typed_like_version_one(&[(PATH, policy)], rows);
}

const POWERS: &str = r#"{
  models: {loan: {amount: "number", rate: "number", years: "number", step: "number?"}},
  blocks: [
    {expression: "loan.factor", value: "(1 + loan.rate / 1200) ^ (-loan.years * 12)"},
    {expression: "loan.payment", value: "round(loan.amount * (loan.rate / 1200) / (1 - loan.factor), 2)"},
    {expression: "loan.grown", value: "loan.amount * 1.01 ^ loan.years"},
    {expression: "loan.stepped", value: "2 ^ (loan.step ?? 3)"},
  ],
}"#;

#[test]
fn powers_with_computed_exponents_run_typed() {
    let rows: Vec<Value> = (0..80)
        .map(|i| json!({"loan": {"amount": 10000 + 997 * i, "rate": 1 + (i % 9), "years": 5 + (i % 26), "step": i % 7}}))
        .collect();
    typed_like_version_one(&[(PATH, POWERS)], rows);
}

#[test]
fn powers_with_fractional_missing_or_huge_exponents_match_version_one() {
    let rows: Vec<Value> = (0..80)
        .map(|i| {
            let mut loan = json!({"amount": 10000 + 997 * i, "rate": 1 + (i % 9), "years": 5 + (i % 26)});
            match i % 6 {
                0 => {}
                1 => loan["step"] = Value::Null,
                2 => loan["step"] = json!(0.5),
                3 => loan["step"] = json!(-2),
                4 => loan["step"] = json!(400),
                _ => loan["step"] = json!(2.25),
            }
            if i % 13 == 0 {
                loan["years"] = json!(2.5);
            }
            json!({"loan": loan})
        })
        .collect();
    columns_match_version_one(&[(PATH, POWERS)], rows);
}

const VALIDATED: &str = r#"{
  models: {trip: {kind: "string(leisure|business)", tags: "string[]", scores: "number[]", flags: "boolean[]", travellers: "traveller[]"}, traveller: {seat: "string(aisle|window)", meals: "string[]"}},
  blocks: [
    {expression: "trip.label", value: "trip.kind + ':' + string(len(trip.tags)) + ':' + string(sum(trip.scores))"},
    {expression: "traveller.picky", value: "len(traveller.meals) > 1 or traveller.seat == 'window'"},
    {expression: "trip.picky", value: "count(trip.travellers, #.picky)"},
  ],
}"#;

#[test]
fn enum_and_primitive_array_inputs_validate_like_version_one() {
    let mut rows: Vec<Value> = (0..80)
        .map(|i| {
            let travellers: Vec<Value> = (0..i % 3)
                .map(|j| json!({"seat": (["aisle", "window"])[(i + j) % 2], "meals": (0..(i + j) % 3).map(|k| format!("m{k}")).collect::<Vec<_>>()}))
                .collect();
            json!({"trip": {"kind": (["leisure", "business"])[i % 2], "tags": (0..i % 4).map(|k| format!("t{k}")).collect::<Vec<_>>(), "scores": (0..i % 3).map(|k| k * 2).collect::<Vec<_>>(), "flags": [i % 2 == 0], "travellers": travellers}})
        })
        .collect();
    rows[3]["trip"]["kind"] = json!("Leisure");
    rows[5]["trip"]["kind"] = json!("");
    rows[7]["trip"]["tags"] = json!(["a", 1]);
    rows[9]["trip"]["tags"] = json!(["a", null, "b"]);
    rows[11]["trip"]["scores"] = json!([1, "2"]);
    rows[13]["trip"]["flags"] = json!([true, null, 0]);
    rows[15]["trip"]["scores"] = json!([null]);
    rows[17]["trip"]["travellers"] = json!([{"seat": "middle", "meals": []}]);
    rows[19]["trip"]["travellers"] = json!([{"seat": "aisle", "meals": ["x", false]}]);
    rows[21]["trip"]["tags"] = json!("not a list");
    columns_match_version_one(&[(PATH, VALIDATED)], rows);
}

const NESTED_VALIDATED: &str = r#"{
  dictionaries: {plan: [["B", "Basic"], ["P", "Plus"]]},
  models: {
    employer: {employees: "employee[]"},
    employee: {salary: "number", dependents: "dependent[]"},
    dependent: {relation: "string(spouse|child)", dob: "date", plan: "plan?", notes: "string[]", pets: "pet[]"},
    pet: {kind: "string", age: "number?"},
  },
  blocks: [
    {expression: "employee.deps", value: "count(employee.dependents, #.relation == 'child' and d().diff(#.dob, 'year') < 26)"},
    {expression: "employer.total", value: "sum(map(employer.employees, #.salary + #.deps * 10))"},
  ],
}"#;

#[test]
fn nested_member_objects_validate_like_version_one() {
    let mut rows: Vec<Value> = (0..60)
        .map(|i| {
            let employees: Vec<Value> = (0..1 + i % 3)
                .map(|j| {
                    let dependents: Vec<Value> = (0..(i + j) % 3)
                        .map(|k| json!({"relation": (["spouse", "child"])[(i + k) % 2], "dob": format!("20{:02}-0{}-1{}", (i + k) % 25, 1 + k % 9, k % 9), "notes": ["a"], "pets": [{"kind": "cat"}]}))
                        .collect();
                    json!({"salary": 1000 + i * 10 + j, "dependents": dependents})
                })
                .collect();
            json!({"employer": {"employees": employees}})
        })
        .collect();
    let deps = |rows: &mut Vec<Value>, at: usize| -> Value {
        rows[at]["employer"]["employees"][0]["dependents"] = json!([{"relation": "child", "dob": "2010-01-01"}]);
        rows[at]["employer"]["employees"][0]["dependents"][0].clone()
    };
    for (at, field, value) in [
        (2, "relation", json!("cousin")),
        (4, "dob", json!("2010-02-30")),
        (6, "dob", json!(12)),
        (8, "plan", json!("X")),
        (10, "plan", json!("P")),
        (12, "notes", json!(["x", 3])),
        (14, "notes", json!("x")),
        (16, "pets", json!([{"kind": 5}])),
        (18, "pets", json!([{"kind": "dog", "age": "old"}])),
        (20, "pets", json!([null, {"kind": "dog", "extra": [1, 2]}])),
        (22, "dob", json!("")),
        (24, "relation", Value::Null),
    ] {
        let mut dependent = deps(&mut rows, at);
        dependent[field] = value;
        rows[at]["employer"]["employees"][0]["dependents"][0] = dependent;
    }
    rows[26]["employer"]["employees"][0]["dependents"] = json!([5]);
    rows[28]["employer"]["employees"][0]["dependents"] = json!([null]);
    columns_match_version_one(&[(PATH, NESTED_VALIDATED)], rows);
}

const OTHER_LISTS: &str = r#"{
  globals: {rules: {hazardous: "string[]"}},
  models: {
    claim: {kind: "string", received: "string[]", travellers: "traveller[]"},
    traveller: {activities: "string[]", age: "number"},
  },
  blocks: [
    {expression: "claim.required", value: "claim.kind == 'theft' ? ['police', 'invoice'] : claim.kind == 'water' ? ['photos'] : []"},
    {expression: "claim.missing", value: "filter(claim.required, not (# in claim.received))"},
    {expression: "claim.count", value: "len(claim.missing)"},
    {expression: "traveller.hazard", value: "some(traveller.activities, # in hazardous)"},
    {expression: "claim.hazards", value: "count(claim.travellers, #.hazard)"},
  ],
}"#;

fn other_list_rows() -> Vec<Value> {
    let kinds = ["theft", "water", "fire"];
    let docs = ["police", "invoice", "photos", "estimate"];
    let acts = ["ski", "dive", "walk", "read"];
    (0..90)
        .map(|i| {
            let received: Vec<&str> = (0..i % 4).map(|k| docs[(i + k) % 4]).collect();
            let travellers: Vec<Value> = (0..i % 3)
                .map(|j| json!({"age": 20 + j, "activities": (0..(i + j) % 3).map(|k| acts[(i + j + k) % 4]).collect::<Vec<_>>()}))
                .collect();
            json!({"hazardous": ["ski", "dive"], "claim": {"kind": kinds[i % 3], "received": received, "travellers": travellers}})
        })
        .collect()
}

#[test]
fn membership_in_another_list_runs_typed() {
    let policy = r#"{
      models: {claim: {required: "string[]", received: "string[]"}},
      blocks: [
        {expression: "claim.missing", value: "filter(claim.required, not (# in claim.received))"},
        {expression: "claim.count", value: "count(claim.required, # in claim.received)"},
      ],
    }"#;
    let docs = ["police", "invoice", "photos", "estimate"];
    let rows: Vec<Value> = (0..80)
        .map(|i| json!({"claim": {"required": (0..i % 3).map(|k| docs[(i + k) % 4]).collect::<Vec<_>>(), "received": (0..i % 4).map(|k| docs[(i * 3 + k) % 4]).collect::<Vec<_>>()}}))
        .collect();
    typed_like_version_one(&[(PATH, policy)], rows);
}

#[test]
fn membership_in_another_list_or_a_global_list_matches_version_one() {
    let mut rows = other_list_rows();
    columns_match_version_one(&[(PATH, OTHER_LISTS)], rows.clone());
    each_goal(OTHER_LISTS, &rows, &[&[], &["claim.missing"], &["traveller.hazard"], &["claim.count", "claim.hazards"]]);
    rows[3]["claim"]["received"] = Value::Null;
    rows[5]["hazardous"] = Value::Null;
    rows[7]["claim"]["received"] = json!(["police", null]);
    rows[9]["claim"]["received"] = json!([1, "police"]);
    columns_match_version_one(&[(PATH, OTHER_LISTS)], rows);
}

const LIFTED_DATES: &str = r#"{
  models: {
    trip: {start: "date", end: "date?", travellers: "traveller[]"},
    traveller: {dob: "date", since: "date?"},
  },
  blocks: [
    {expression: "traveller.age", value: "trip.start.diff(traveller.dob, 'year')"},
    {expression: "traveller.member", value: "traveller.since == null ? 0 : trip.start.diff(traveller.since, 'day')"},
    {expression: "traveller.late", value: "trip.end != null and traveller.dob < trip.end"},
    {expression: "trip.oldest", value: "max(map(trip.travellers, #.age))"},
  ],
}"#;

#[test]
fn member_blocks_reading_root_dates_run_typed() {
    let rows: Vec<Value> = (0..80)
        .map(|i| {
            let travellers: Vec<Value> = (0..1 + i % 3)
                .map(|j| json!({"dob": format!("19{:02}-0{}-1{}", 50 + (i + j) % 49, 1 + j % 9, i % 9), "since": format!("2020-0{}-01", 1 + (i + j) % 9)}))
                .collect();
            json!({"trip": {"start": format!("2025-0{}-1{}", 1 + i % 9, i % 10), "end": format!("2025-1{}-01", i % 3), "travellers": travellers}})
        })
        .collect();
    typed_like_version_one(&[(PATH, LIFTED_DATES)], rows);
}

#[test]
fn member_blocks_reading_root_dates_in_any_form_match_version_one() {
    let starts = ["2025-03-20", "2025-03-20T23:30:00-05:00", "2024-02-29T12:00:00Z", "2025-01-01 10:00", "2025-13-01", ""];
    let rows: Vec<Value> = (0..72)
        .map(|i| {
            let travellers: Vec<Value> = (0..i % 3)
                .map(|j| {
                    let mut t = json!({"dob": (["1990-05-05", "2000-02-29T08:00:00+05:30", "1985-12-31"])[(i + j) % 3]});
                    if (i + j) % 4 == 0 {
                        t["since"] = Value::Null;
                    }
                    t
                })
                .collect();
            let mut trip = json!({"start": starts[i % starts.len()], "travellers": travellers});
            if i % 5 != 0 {
                trip["end"] = json!(starts[(i + 2) % starts.len()]);
            }
            json!({"trip": trip})
        })
        .collect();
    columns_match_version_one(&[(PATH, LIFTED_DATES)], rows);
}

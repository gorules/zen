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

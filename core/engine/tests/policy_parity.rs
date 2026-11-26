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
    let path: Arc<str> = Arc::from(PATH);
    let output = new
        .evaluate_columns_pinned(&path, &[], &columns, &options())
        .expect("columnar policy");
    for at in 0..rows.len() {
        let input = columns.row(at);
        let want = old
            .evaluate_pinned(&request(input.depth_clone(usize::MAX)), &options())
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

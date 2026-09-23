use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use zen_engine::model::DecisionContent;
use zen_engine::policy::{
    Cursor, CursorTarget, EvaluateRequest, PolicyWorkspace, ReferenceKind, RenameTarget,
    ScopeRequest,
};
use zen_expression::variable::{Variable, VariableType};

const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/policy/fixtures/");

fn build_workspace(policies: &[String]) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    for path in policies {
        let raw = std::fs::read_to_string(format!("{FIXTURES_DIR}{path}"))
            .unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"));
        let doc = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("cannot deserialize fixture {path}: {e}"));
        ws.set_policy(path.as_str(), doc);
    }
    ws
}

fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).expect("toml converts to json")
}

fn numbers_equal(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() <= f64::EPSILON * x.abs().max(y.abs()).max(1.0),
        _ => a == b,
    }
}

fn assert_subset(expected: &serde_json::Value, actual: Option<&serde_json::Value>, ctx: &str) {
    use serde_json::Value;
    let Some(actual) = actual else {
        panic!("{ctx}: expected {expected}, but key is absent");
    };
    match (expected, actual) {
        (Value::Object(exp), Value::Object(act)) => {
            for (key, val) in exp {
                assert_subset(val, act.get(key), &format!("{ctx}.{key}"));
            }
        }
        (Value::Array(exp), Value::Array(act)) => {
            assert_eq!(
                exp.len(),
                act.len(),
                "{ctx}: expected {} elements, got {}: {actual}",
                exp.len(),
                act.len()
            );
            for (idx, val) in exp.iter().enumerate() {
                assert_subset(val, act.get(idx), &format!("{ctx}[{idx}]"));
            }
        }
        (Value::Number(exp), Value::Number(act)) => {
            assert!(
                numbers_equal(exp, act),
                "{ctx}: expected {expected}, got {actual}"
            );
        }
        _ => assert_eq!(expected, actual, "{ctx}: expected {expected}, got {actual}"),
    }
}

fn subset_matches(expected: &serde_json::Value, actual: Option<&serde_json::Value>) -> bool {
    use serde_json::Value;
    let Some(actual) = actual else { return false };
    match (expected, actual) {
        (Value::Object(exp), Value::Object(act)) => exp
            .iter()
            .all(|(key, val)| subset_matches(val, act.get(key))),
        (Value::Array(exp), Value::Array(act)) => {
            exp.len() == act.len() && exp.iter().zip(act).all(|(e, a)| subset_matches(e, Some(a)))
        }
        (Value::Number(exp), Value::Number(act)) => numbers_equal(exp, act),
        _ => expected == actual,
    }
}

#[derive(Debug, Deserialize)]
struct EvaluationFile {
    test: Vec<EvaluationCase>,
}

#[derive(Debug, Deserialize)]
struct EvaluationCase {
    name: String,
    policies: Vec<String>,
    input: toml::Value,
    output: toml::Value,
    #[serde(default)]
    nulls: Vec<String>,
    trace: Option<TraceExpectation>,
}

#[derive(Debug, Deserialize)]
struct TraceExpectation {
    blocks: BTreeMap<String, toml::Value>,
}

fn normalize_block_expectation(value: &toml::Value) -> serde_json::Value {
    let mut json = toml_to_json(value);
    if let Some(obj) = json.as_object_mut() {
        if let Some(rows) = obj.remove("matched_rows") {
            obj.insert("matchedRows".into(), rows);
        }
    }
    json
}

fn run_evaluation(file_name: &str, toml_data: &str) {
    let file: EvaluationFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("cannot parse {file_name}: {e}"));

    for test in &file.test {
        let ctx = format!("[{file_name}:{}]", test.name);
        let ws = build_workspace(&test.policies);
        let req = EvaluateRequest {
            policy_path: Arc::from(test.policies[0].as_str()),
            input: Variable::from(toml_to_json(&test.input)),
            goals: Vec::new(),
            trace: test.trace.is_some(),
        };
        let result = ws
            .evaluate(&req)
            .unwrap_or_else(|e| panic!("{ctx} evaluation failed: {e:?}"));

        let output_json: serde_json::Value = result.output.into();
        assert_subset(&toml_to_json(&test.output), Some(&output_json), &ctx);
        for path in &test.nulls {
            let pointer = format!("/{}", path.replace('.', "/"));
            assert_eq!(
                output_json.pointer(&pointer),
                Some(&serde_json::Value::Null),
                "{ctx} expected explicit null at '{path}'; got {output_json}"
            );
        }

        let Some(expected_trace) = &test.trace else {
            continue;
        };
        let trace = result
            .trace
            .unwrap_or_else(|| panic!("{ctx} expected trace but got none"));
        for (block_id, expectation) in &expected_trace.blocks {
            let expected = normalize_block_expectation(expectation);
            let candidates: Vec<serde_json::Value> = trace
                .executions
                .iter()
                .filter(|e| e.block_id.as_ref() == block_id.as_str())
                .map(|e| serde_json::to_value(&e.trace).expect("trace serializes"))
                .collect();
            assert!(
                !candidates.is_empty(),
                "{ctx} block '{block_id}' not found in trace; executed: {:?}",
                trace
                    .executions
                    .iter()
                    .map(|e| e.block_id.as_ref())
                    .collect::<Vec<_>>()
            );
            assert!(
                candidates
                    .iter()
                    .any(|actual| subset_matches(&expected, Some(actual))),
                "{ctx} block '{block_id}': no execution matches {expected}\n  got: {candidates:#?}"
            );
        }
    }
}

#[derive(Debug, Deserialize)]
struct RenameFile {
    policies: Vec<String>,
    test: Vec<RenameCase>,
}

#[derive(Debug, Deserialize)]
struct RenameCase {
    name: String,
    entity: String,
    field: String,
    new_name: String,
    edits: Vec<EditExpectation>,
}

#[derive(Debug, Deserialize)]
struct EditExpectation {
    policy: String,
    block_id: String,
    expression_id: Option<String>,
    target_kind: Option<String>,
}

fn rename_target(entity: &str, field: &str) -> RenameTarget {
    if field.is_empty() {
        RenameTarget::Entity {
            name: Arc::from(entity),
        }
    } else {
        RenameTarget::Field {
            entity: Arc::from(entity),
            field: Arc::from(field),
        }
    }
}

fn run_rename(file_name: &str, toml_data: &str) {
    let file: RenameFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("cannot parse {file_name}: {e}"));
    let ws = build_workspace(&file.policies);

    for test in &file.test {
        let ctx = format!("[{file_name}:{}]", test.name);
        let target = rename_target(&test.entity, &test.field);

        let sites = ws.references(&target);
        let mut unmatched: Vec<usize> = (0..sites.len()).collect();
        for expected in &test.edits {
            let pos = unmatched.iter().position(|&i| {
                let site = &sites[i];
                if site.policy_path.as_ref() != expected.policy
                    || site.block_id.as_ref() != expected.block_id
                {
                    return false;
                }
                match (&expected.expression_id, &expected.target_kind) {
                    (Some(id), _) => {
                        site.expression_id.as_deref() == Some(id.as_str())
                            && site.kind != ReferenceKind::DataModel
                    }
                    (None, Some(_)) => site.kind == ReferenceKind::DataModel,
                    (None, None) => {
                        site.expression_id.is_none() && site.kind != ReferenceKind::DataModel
                    }
                }
            });
            match pos {
                Some(idx_pos) => {
                    unmatched.remove(idx_pos);
                }
                None => panic!(
                    "{ctx} expected site {expected:?} not found.\n  got: {:#?}",
                    sites
                ),
            }
        }
        assert!(
            unmatched.is_empty(),
            "{ctx} unexpected extra sites: {:#?}",
            unmatched.iter().map(|&i| &sites[i]).collect::<Vec<_>>()
        );

        let edits = ws.rename(&target, &test.new_name);
        if test.edits.is_empty() {
            assert!(edits.is_empty(), "{ctx} expected no edits, got {edits:?}");
        } else {
            let serialized = serde_json::to_string(&edits).expect("edits serialize");
            assert!(
                serialized.contains(&test.new_name),
                "{ctx} rename edits must contain '{}': {serialized}",
                test.new_name
            );
        }
    }
}

#[derive(Debug, Deserialize)]
struct PrepareRenameFile {
    policies: Vec<String>,
    test: Vec<PrepareRenameCase>,
}

#[derive(Debug, Deserialize)]
struct PrepareRenameCase {
    name: String,
    policy: String,
    block_id: String,
    expression_id: String,
    pos: u32,
    expected_entity: Option<String>,
    expected_field: Option<String>,
}

fn run_prepare_rename(file_name: &str, toml_data: &str) {
    let file: PrepareRenameFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("cannot parse {file_name}: {e}"));
    let ws = build_workspace(&file.policies);

    for test in &file.test {
        let ctx = format!("[{file_name}:{}]", test.name);
        let result = ws.prepare_rename(&Cursor {
            policy_path: Arc::from(test.policy.as_str()),
            block_id: Arc::from(test.block_id.as_str()),
            pos: test.pos,
            target: CursorTarget::Expression {
                id: Arc::from(test.expression_id.as_str()),
            },
        });

        match (&test.expected_entity, result) {
            (None, None) => {}
            (None, Some(r)) => panic!("{ctx} expected None, got {:?}", r.target),
            (Some(_), None) => panic!("{ctx} expected Some, got None"),
            (Some(entity), Some(r)) => {
                let expected_field = test.expected_field.as_deref().unwrap_or("");
                let expected = rename_target(entity, expected_field);
                assert_eq!(r.target, expected, "{ctx} target mismatch");
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct CompletionsFile {
    policies: Vec<String>,
    test: Vec<CompletionsCase>,
}

#[derive(Debug, Deserialize)]
struct CompletionsCase {
    name: String,
    policy: String,
    block_id: String,
    expression_id: String,
    pos: u32,
    #[serde(default)]
    head: bool,
    row: Option<String>,
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    excludes: Vec<String>,
}

fn run_completions(file_name: &str, toml_data: &str) {
    let file: CompletionsFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("cannot parse {file_name}: {e}"));
    let ws = build_workspace(&file.policies);

    for test in &file.test {
        let ctx = format!("[{file_name}:{}]", test.name);
        let target = if test.head {
            CursorTarget::DecisionTableHead {
                col: Arc::from(test.expression_id.as_str()),
            }
        } else if let Some(row) = &test.row {
            CursorTarget::DecisionTableCell {
                row: Arc::from(row.as_str()),
                col: Arc::from(test.expression_id.as_str()),
            }
        } else {
            CursorTarget::Expression {
                id: Arc::from(test.expression_id.as_str()),
            }
        };
        let completions = ws.completions(&Cursor {
            policy_path: Arc::from(test.policy.as_str()),
            block_id: Arc::from(test.block_id.as_str()),
            pos: test.pos,
            target,
        });
        let labels: Vec<&str> = completions.iter().map(|c| c.label.as_ref()).collect();

        for inc in &test.includes {
            assert!(
                labels.contains(&inc.as_str()),
                "{ctx} expected completion '{inc}' missing.\n  got: {labels:?}"
            );
        }
        for exc in &test.excludes {
            assert!(
                !labels.contains(&exc.as_str()),
                "{ctx} unexpected completion '{exc}'.\n  got: {labels:?}"
            );
        }
    }
}

#[derive(Debug, Deserialize)]
struct SlotsFile {
    policies: Vec<String>,
    #[serde(default)]
    graphs: Vec<String>,
    test: Vec<SlotsCase>,
}

#[derive(Debug, Deserialize)]
struct SlotsCase {
    name: String,
    policy: String,
    block_id: String,
    target: String,
    id: Option<String>,
    row: Option<String>,
    kind: Option<String>,
    role: Option<String>,
    expected: Option<String>,
    subject: Option<String>,
    text: Option<String>,
    state: Option<String>,
    options: Option<Vec<String>>,
}

impl SlotsCase {
    fn cursor(&self) -> Cursor {
        let id = || Arc::from(self.id.as_deref().expect("target id"));
        let target = match self.target.as_str() {
            "expression" => CursorTarget::Expression { id: id() },
            "cell" => CursorTarget::DecisionTableCell {
                row: Arc::from(self.row.as_deref().expect("row")),
                col: id(),
            },
            "head" => CursorTarget::DecisionTableHead { col: id() },
            "expression_key" => CursorTarget::ExpressionKey,
            "assertion_output" => CursorTarget::AssertionOutput,
            "match_target" => CursorTarget::MatchTarget,
            "match_value" => CursorTarget::MatchValue { id: id() },
            "transform_input" => CursorTarget::TransformInput,
            "data_model_name" => CursorTarget::DataModelName,
            other => panic!("unknown target {other}"),
        };
        Cursor {
            policy_path: Arc::from(self.policy.as_str()),
            block_id: Arc::from(self.block_id.as_str()),
            pos: 0,
            target,
        }
    }

    fn check(&self, ws: &PolicyWorkspace) {
        let ctx = format!("[slots.toml:{}]", self.name);
        let mut cursor = self.cursor();
        let Some(scope) = ws.cursor_scope(&cursor) else {
            assert!(self.kind.is_none(), "{ctx} expected a scope, got none");
            return;
        };
        let render = |t: Option<VariableType>| t.map(|t| t.to_string()).unwrap_or_default();
        let actual = [
            ("kind", format!("{:?}", scope.kind).to_lowercase()),
            (
                "role",
                serde_json::to_value(scope.role)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
            ),
            ("expected", render(scope.expected.clone())),
            ("subject", render(scope.subject_type())),
        ];
        let wanted = [&self.kind, &self.role, &self.expected, &self.subject];
        for ((field, actual), wanted) in actual.iter().zip(wanted) {
            if let Some(wanted) = wanted {
                assert_eq!(actual, wanted, "{ctx} {field}");
            }
        }
        let Some(text) = &self.text else {
            return;
        };
        let caret = text.find('|').expect("caret");
        let source = text.replacen('|', "", 1);
        cursor.pos = source[..caret].encode_utf16().count() as u32;
        let slot = ws.slot(&cursor, &source).expect("slot").slot;
        if let Some(state) = &self.state {
            assert_eq!(
                &serde_json::to_value(slot.state).unwrap(),
                state,
                "{ctx} state"
            );
        }
        if let Some(options) = &self.options {
            let actual: Vec<String> = slot
                .options
                .iter()
                .map(|o| match o.label == o.value {
                    true => o.value.clone(),
                    false => format!("{}={}", o.value, o.label),
                })
                .collect();
            assert_eq!(&actual, options, "{ctx} options");
        }
    }
}

fn run_slots(file_name: &str, toml_data: &str) {
    let file: SlotsFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("cannot parse {file_name}: {e}"));
    let mut ws = build_workspace(&file.policies);
    for path in &file.graphs {
        let raw = std::fs::read_to_string(format!("{FIXTURES_DIR}{path}"))
            .unwrap_or_else(|e| panic!("cannot read fixture {path}: {e}"));
        let doc: DecisionContent = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("cannot deserialize fixture {path}: {e}"));
        ws.set_document(path.as_str(), doc);
    }
    for test in &file.test {
        test.check(&ws);
    }
}

#[derive(Debug, Deserialize)]
struct EntitiesFile {
    policies: Vec<String>,
    test: Vec<EntitiesCase>,
}

#[derive(Debug, Deserialize)]
struct EntitiesCase {
    name: String,
    policy: String,
    entity_count: Option<usize>,
    entity: Option<String>,
    field: Option<String>,
    field_kind: Option<toml::Value>,
    instance_of: Option<toml::Value>,
    #[serde(default)]
    no_instance_of: bool,
    property_kind: Option<String>,
    source: Option<String>,
    source_is_local: Option<bool>,
    #[serde(default)]
    no_duplicate_fields: bool,
    #[serde(default)]
    absent_fields: Vec<String>,
}

fn run_entities(file_name: &str, toml_data: &str) {
    let file: EntitiesFile =
        toml::from_str(toml_data).unwrap_or_else(|e| panic!("cannot parse {file_name}: {e}"));
    let ws = build_workspace(&file.policies);

    for test in &file.test {
        let ctx = format!("[{file_name}:{}]", test.name);
        let entities = ws.entities(&ScopeRequest::for_policy(test.policy.as_str()));

        if let Some(count) = test.entity_count {
            assert_eq!(
                entities.len(),
                count,
                "{ctx} entity count mismatch; got {:?}",
                entities.iter().map(|e| e.name.as_ref()).collect::<Vec<_>>()
            );
        }
        let Some(entity) = &test.entity else {
            continue;
        };
        let entity_obj = entities
            .iter()
            .find(|e| e.name.as_ref() == entity.as_str())
            .unwrap_or_else(|| panic!("{ctx} entity '{entity}' not found"));

        if test.no_duplicate_fields {
            let mut names: Vec<&str> = entity_obj.fields.iter().map(|f| f.name.as_ref()).collect();
            let total = names.len();
            names.sort();
            names.dedup();
            assert_eq!(names.len(), total, "{ctx} duplicate fields present");
        }
        for absent in &test.absent_fields {
            assert!(
                !entity_obj
                    .fields
                    .iter()
                    .any(|f| f.name.as_ref() == absent.as_str()),
                "{ctx} field '{absent}' should be absent"
            );
        }
        let Some(field) = &test.field else {
            continue;
        };
        let field_obj = entity_obj
            .fields
            .iter()
            .find(|f| f.name.as_ref() == field.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "{ctx} field '{field}' not found; got {:?}",
                    entity_obj
                        .fields
                        .iter()
                        .map(|f| f.name.as_ref())
                        .collect::<Vec<_>>()
                )
            });

        let origin_json = serde_json::to_value(&field_obj.origin).expect("origin serializes");
        if let Some(kind) = &test.property_kind {
            let actual = match origin_json["origin"].as_str() {
                Some("schema") => "input",
                Some("computed") => "computed",
                other => panic!("{ctx} unexpected origin {other:?}"),
            };
            assert_eq!(actual, kind, "{ctx} property_kind mismatch: {origin_json}");
        }
        if let Some(expected_kind) = &test.field_kind {
            assert_eq!(
                origin_json["origin"].as_str(),
                Some("schema"),
                "{ctx} field_kind asserts require a schema-origin field: {origin_json}"
            );
            assert_subset(
                &toml_to_json(expected_kind),
                Some(&origin_json["fieldKind"]),
                &format!("{ctx} field_kind"),
            );
        }
        if let Some(expected) = &test.instance_of {
            assert_eq!(
                origin_json["origin"].as_str(),
                Some("computed"),
                "{ctx} instance_of asserts require a computed field: {origin_json}"
            );
            assert_subset(
                &toml_to_json(expected),
                Some(&origin_json["instanceOf"]),
                &format!("{ctx} instance_of"),
            );
        }
        if test.no_instance_of {
            assert!(
                origin_json.get("instanceOf").is_none(),
                "{ctx} expected no instanceOf: {origin_json}"
            );
        }

        let source_policy = match origin_json["origin"].as_str() {
            Some("schema") => origin_json["source"].as_str(),
            Some("computed") => origin_json["writtenBy"]["policyPath"].as_str(),
            _ => None,
        };
        if let Some(expected) = &test.source {
            assert_eq!(
                source_policy,
                Some(expected.as_str()),
                "{ctx} source mismatch: {origin_json}"
            );
        }
        if let Some(local) = test.source_is_local {
            assert_eq!(
                source_policy == Some(test.policy.as_str()),
                local,
                "{ctx} source_is_local mismatch: {origin_json}"
            );
        }
    }
}

#[test]
fn evaluation_toml_cases() {
    run_evaluation(
        "evaluation.toml",
        include_str!("data/policy/evaluation.toml"),
    );
}

#[test]
fn rename_toml_cases() {
    run_rename("rename.toml", include_str!("data/policy/rename.toml"));
}

#[test]
fn rename_multi_policy_toml_cases() {
    run_rename(
        "rename_multi_policy.toml",
        include_str!("data/policy/rename_multi_policy.toml"),
    );
}

#[test]
fn prepare_rename_toml_cases() {
    run_prepare_rename(
        "prepare_rename.toml",
        include_str!("data/policy/prepare_rename.toml"),
    );
}

#[test]
fn completions_toml_cases() {
    run_completions(
        "completions.toml",
        include_str!("data/policy/completions.toml"),
    );
}

#[test]
fn completions_scoping_toml_cases() {
    run_completions(
        "completions_scoping.toml",
        include_str!("data/policy/completions_scoping.toml"),
    );
}

#[test]
fn completions_multi_entity_toml_cases() {
    run_completions(
        "completions_multi_entity.toml",
        include_str!("data/policy/completions_multi_entity.toml"),
    );
}

#[test]
fn entities_toml_cases() {
    run_entities("entities.toml", include_str!("data/policy/entities.toml"));
}

#[test]
fn entities_filter_toml_cases() {
    run_entities(
        "entities_filter.toml",
        include_str!("data/policy/entities_filter.toml"),
    );
}

#[test]
fn entities_merge_toml_cases() {
    run_entities(
        "entities_merge.toml",
        include_str!("data/policy/entities_merge.toml"),
    );
}

#[test]
fn entities_merge_multi_policy_toml_cases() {
    run_entities(
        "entities_merge_multi_policy.toml",
        include_str!("data/policy/entities_merge_multi_policy.toml"),
    );
}

#[test]
fn slots_toml_cases() {
    run_slots("slots.toml", include_str!("data/policy/slots.toml"));
}

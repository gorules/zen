use serde::Deserialize;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use zen_engine::policy::{
    Cursor, CursorTarget, EngineEdit, EvaluateRequest, EvaluationError, PolicyWorkspace,
    ScopeRequest, Severity,
};

fn rewritten_block_json(edits: &[EngineEdit], block_id: &str) -> Option<String> {
    edits.iter().find_map(|e| match e {
        EngineEdit::ReplaceBlock {
            block_id: bid,
            new_block,
            ..
        } if bid.as_ref() == block_id => Some(serde_json::to_string(new_block).unwrap()),
        _ => None,
    })
}

fn any_rewritten_contains(edits: &[EngineEdit], needle: &str) -> bool {
    edits.iter().any(|e| match e {
        EngineEdit::ReplaceBlock { new_block, .. } => {
            serde_json::to_string(new_block).unwrap().contains(needle)
        }
        _ => false,
    })
}

fn any_touches_block(edits: &[EngineEdit], block_id: &str) -> bool {
    edits.iter().any(|e| match e {
        EngineEdit::ReplaceBlock { block_id: bid, .. }
        | EngineEdit::DeleteBlock { block_id: bid, .. } => bid.as_ref() == block_id,
        EngineEdit::InsertBlock { .. } => false,
    })
}
use zen_expression::variable::{Variable, VariableType};

#[derive(Deserialize, Debug)]
struct DiagnosticsCase {
    name: String,
    #[serde(default)]
    policies: Vec<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    policy: Option<String>,
    #[serde(default)]
    no_errors: bool,
    #[serde(default)]
    error_codes: Vec<String>,
    #[serde(default)]
    warning_codes: Vec<String>,
    #[serde(default)]
    hint_codes: Vec<String>,
    #[serde(default)]
    error_count: Option<usize>,
    #[serde(default)]
    hint_count: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct DiagnosticsFile {
    #[serde(rename = "test")]
    tests: Vec<DiagnosticsCase>,
}

impl DiagnosticsCase {
    fn policy_path(&self) -> &str {
        self.policy
            .as_deref()
            .or_else(|| self.policies.first().map(String::as_str))
            .unwrap_or("p")
    }

    fn run(&self, fixtures_dir: &Path) {
        let mut ws = PolicyWorkspace::new();

        if let Some(inline) = &self.content {
            let doc: serde_json::Value = serde_json::from_str(inline)
                .unwrap_or_else(|e| panic!("[{}] invalid inline policy JSON: {e}", self.name));
            let path = self.policy_path();
            ws.set_policy(path, serde_json::from_value(doc).unwrap());
        } else {
            for fixture in &self.policies {
                let path = fixtures_dir.join(fixture);
                let raw = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("[{}] cannot read {:?}: {e}", self.name, path));
                let doc: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
                    panic!("[{}] invalid fixture JSON in {fixture}: {e}", self.name)
                });
                ws.set_policy(fixture.as_str(), serde_json::from_value(doc).unwrap());
            }
        }

        let target = self.policy_path();
        let diagnostics = ws.diagnostics(target);
        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        let warnings: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .collect();
        let hints: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Hint)
            .collect();

        if self.no_errors {
            assert!(
                errors.is_empty(),
                "[{}] expected no errors, got {errors:#?}",
                self.name,
            );
        }
        if let Some(expected) = self.error_count {
            assert_eq!(
                errors.len(),
                expected,
                "[{}] expected exactly {expected} error(s), got {}: {errors:#?}",
                self.name,
                errors.len(),
            );
        }
        for code in &self.error_codes {
            assert!(
                errors.iter().any(|d| format!("{:?}", d.code) == *code),
                "[{}] expected error `{code}` not found in {errors:#?}",
                self.name,
            );
        }
        for code in &self.warning_codes {
            assert!(
                warnings.iter().any(|d| format!("{:?}", d.code) == *code),
                "[{}] expected warning `{code}` not found in {warnings:#?}",
                self.name,
            );
        }
        if let Some(expected) = self.hint_count {
            assert_eq!(
                hints.len(),
                expected,
                "[{}] expected exactly {expected} hint(s), got {}: {hints:#?}",
                self.name,
                hints.len(),
            );
        }
        for code in &self.hint_codes {
            assert!(
                hints.iter().any(|d| format!("{:?}", d.code) == *code),
                "[{}] expected hint `{code}` not found in {hints:#?}",
                self.name,
            );
        }
    }
}

#[test]
fn diagnostics_toml_cases() {
    let toml_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/policy/diagnostics.toml");
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/policy/fixtures");
    let raw =
        fs::read_to_string(&toml_path).unwrap_or_else(|e| panic!("cannot read {toml_path:?}: {e}"));
    let file: DiagnosticsFile =
        toml::from_str(&raw).unwrap_or_else(|e| panic!("cannot parse {toml_path:?}: {e}"));

    let mut failures: Vec<String> = Vec::new();
    for case in &file.tests {
        if let Err(err) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| case.run(&fixtures_dir)))
        {
            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic>".to_string());
            failures.push(format!("{}:\n{msg}", case.name));
        }
    }
    assert!(
        failures.is_empty(),
        "{} diagnostics case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

#[derive(Deserialize, Debug)]
struct DependencyCase {
    name: String,
    #[serde(default)]
    policies: Vec<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    policy: Option<String>,
    target: String,
    #[serde(default)]
    computed: Vec<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    unresolved: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct DependencyFile {
    #[serde(rename = "test")]
    tests: Vec<DependencyCase>,
}

impl DependencyCase {
    fn collect(node: &zen_engine::policy::DependencyNode, out: &mut Vec<(String, bool, bool)>) {
        out.push((
            node.property.to_string(),
            node.written_by.is_some(),
            node.unresolved,
        ));
        for d in &node.deps {
            Self::collect(d, out);
        }
    }

    fn run(&self, fixtures_dir: &Path) {
        let mut ws = PolicyWorkspace::new();
        if let Some(inline) = &self.content {
            let doc: serde_json::Value = serde_json::from_str(inline)
                .unwrap_or_else(|e| panic!("[{}] invalid inline policy JSON: {e}", self.name));
            let path = self
                .policy
                .as_deref()
                .or_else(|| self.policies.first().map(String::as_str))
                .unwrap_or("p");
            ws.set_policy(path, serde_json::from_value(doc).unwrap());
        } else {
            for fixture in &self.policies {
                let path = fixtures_dir.join(fixture);
                let raw = fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("[{}] cannot read {:?}: {e}", self.name, path));
                let doc: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
                    panic!("[{}] invalid fixture JSON in {fixture}: {e}", self.name)
                });
                ws.set_policy(fixture.as_str(), serde_json::from_value(doc).unwrap());
            }
        }

        let mut nodes: Vec<(String, bool, bool)> = Vec::new();
        Self::collect(&ws.dependencies(&self.target), &mut nodes);
        let paths: Vec<&str> = nodes.iter().map(|(p, _, _)| p.as_str()).collect();
        let find = |path: &str| nodes.iter().find(|(p, _, _)| p == path);

        for c in &self.computed {
            let node = find(c)
                .unwrap_or_else(|| panic!("[{}] expected `{c}` in tree; got {paths:?}", self.name));
            assert!(
                node.1,
                "[{}] `{c}` expected computed (writer), got input/unresolved",
                self.name
            );
        }
        for i in &self.inputs {
            let node = find(i)
                .unwrap_or_else(|| panic!("[{}] expected `{i}` in tree; got {paths:?}", self.name));
            assert!(
                !node.1 && !node.2,
                "[{}] `{i}` expected plain input, got writer={} unresolved={}",
                self.name,
                node.1,
                node.2
            );
        }
        for u in &self.unresolved {
            let node = find(u)
                .unwrap_or_else(|| panic!("[{}] expected `{u}` in tree; got {paths:?}", self.name));
            assert!(
                node.2,
                "[{}] `{u}` expected unresolved, got writer={}",
                self.name, node.1
            );
        }
    }
}

#[test]
fn dependencies_toml_cases() {
    let toml_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/policy/dependencies.toml");
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/policy/fixtures");
    let raw =
        fs::read_to_string(&toml_path).unwrap_or_else(|e| panic!("cannot read {toml_path:?}: {e}"));
    let file: DependencyFile =
        toml::from_str(&raw).unwrap_or_else(|e| panic!("cannot parse {toml_path:?}: {e}"));

    let mut failures: Vec<String> = Vec::new();
    for case in &file.tests {
        if let Err(err) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| case.run(&fixtures_dir)))
        {
            let msg = err
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "<non-string panic>".to_string());
            failures.push(format!("{}:\n{msg}", case.name));
        }
    }
    assert!(
        failures.is_empty(),
        "{} dependency case(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

fn data_model_document() -> serde_json::Value {
    json!({
        "blocks": [
            {
                "id": "dm-heading",
                "type": "heading",
                "props": {},
                "children": []
            },
            {
                "id": "dm-customer",
                "type": "dataModel",
                "props": {
                    "data": json!({
                        "name": "customer",
                        "properties": [
                            { "id": "p1", "name": "name", "type": "string", "array": false, "optional": false },
                            { "id": "p2", "name": "age", "type": "number", "array": false, "optional": false },
                            { "id": "p3", "name": "country", "type": "string", "array": false, "optional": false },
                            { "id": "p4", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false },
                            { "id": "p5", "name": "creditReport", "type": "relationship", "target": "creditReport", "array": false, "optional": false },
                            { "id": "p6", "name": "favoriteProduct", "type": "reference", "target": "product", "array": false, "optional": true }
                        ]
                    })
                }
            },
            {
                "id": "dm-company",
                "type": "dataModel",
                "props": {
                    "data": json!({
                        "name": "company",
                        "properties": [
                            { "id": "p7", "name": "id", "type": "string", "array": false, "optional": false },
                            { "id": "p8", "name": "revenue", "type": "number", "array": false, "optional": false }
                        ]
                    })
                }
            },
            {
                "id": "dm-credit-report",
                "type": "dataModel",
                "props": {
                    "data": json!({
                        "name": "creditReport",
                        "properties": [
                            { "id": "p11", "name": "score", "type": "number", "array": false, "optional": false }
                        ]
                    })
                }
            },
            {
                "id": "dm-product",
                "type": "dataModel",
                "props": {
                    "data": json!({
                        "name": "product",
                        "properties": [
                            { "id": "p14", "name": "id", "type": "string", "array": false, "optional": false },
                            { "id": "p15", "name": "name", "type": "string", "array": false, "optional": false }
                        ]
                    })
                }
            }
        ]
    })
}

#[test]
fn basic_entities_and_inputs() {
    let mut ws = PolicyWorkspace::new();
    let doc = data_model_document();
    ws.set_policy("test", serde_json::from_value(doc).unwrap());

    let diags = ws.diagnostics("test");
    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {:?}", errors);

    let entities = ws.entities(&ScopeRequest::for_policy("test"));
    let names: Vec<_> = entities.iter().map(|e| e.name.to_string()).collect();
    assert!(names.contains(&"customer".to_string()));
    assert!(names.contains(&"company".to_string()));
    assert!(names.contains(&"creditReport".to_string()));
    assert!(names.contains(&"product".to_string()));

    let customer = entities
        .iter()
        .find(|e| e.name.as_ref() == "customer")
        .unwrap();
    assert_eq!(customer.fields.len(), 6);

    let inputs = ws.inputs(&ScopeRequest::for_policy("test"));
    let paths: Vec<_> = inputs.iter().map(|p| p.path.to_string()).collect();

    let fav = inputs
        .iter()
        .find(|p| p.path.as_ref() == "customer.favoriteProduct");
    assert!(
        fav.is_some(),
        "customer.favoriteProduct missing; inputs={:?}",
        paths
    );
    assert!(matches!(fav.unwrap().resolved_type, VariableType::String));

    let companies = inputs
        .iter()
        .find(|p| p.path.as_ref() == "customer.companies");
    assert!(companies.is_some());
    assert!(matches!(
        companies.unwrap().resolved_type,
        VariableType::Array(_)
    ));

    let product = inputs.iter().find(|p| p.path.as_ref() == "product");
    assert!(product.is_some(), "product top-level ref target missing");
    assert!(matches!(
        product.unwrap().resolved_type,
        VariableType::Array(_)
    ));

    assert!(
        !paths.iter().any(|p| p.starts_with("company.")),
        "company.* should not be top-level"
    );
    assert!(
        !paths.iter().any(|p| p == "company"),
        "company should not appear as top-level"
    );
}

#[test]
fn references_sorted_dataModel_then_reads_then_writes() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "policy",
        serde_json::from_value(analysis_document()).unwrap(),
    );

    let refs = ws.references(&zen_engine::policy::RenameTarget::Field {
        entity: Arc::from("customer"),
        field: Arc::from("creditTier"),
    });
    assert!(!refs.is_empty(), "expected at least one reference site");

    let order: Vec<u8> = refs.iter().map(|r| r.kind.display_order()).collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(
        order, sorted,
        "references should be sorted by kind: dataModel → expressionRead → writeKey; got {refs:#?}",
    );
}

#[test]
fn goal_evaluate_accepts_array_input_for_array_element_reads() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "policy",
        serde_json::from_value(analysis_document()).unwrap(),
    );

    let skeleton = ws.input_skeleton(&ScopeRequest {
        policy_path: Arc::from("policy"),
        goals: vec![Arc::from("customer.totalRevenue")],
    });
    let customer = skeleton
        .as_object()
        .and_then(|o| o.get("customer"))
        .and_then(|v| v.as_object())
        .expect("customer in skeleton");
    assert!(
        customer.contains_key("companies"),
        "skeleton must include customer.companies for goal=totalRevenue; got {customer:#?}",
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("policy"),
        input: Variable::from(skeleton),
        goals: vec![Arc::from("customer.totalRevenue")],
        trace: false,
    };
    let result = ws.evaluate(&req);
    assert!(
        result.is_ok(),
        "evaluate(goal=totalRevenue, input=skeleton) should succeed; got {:?}",
        result.err(),
    );
}

fn optional_input_document() -> serde_json::Value {
    json!({
        "blocks": [
            {
                "id": "dm",
                "type": "dataModel",
                "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false },
                        { "id": "p2", "name": "nickname", "type": "string", "array": false, "optional": true }
                    ]
                }},
                "children": []
            },
            {
                "id": "assert-adult",
                "type": "assertion",
                "props": { "data": {
                    "output": "customer.adult",
                    "conditions": [
                        { "id": "c1", "expression": "customer.age >= 18", "operator": "and", "depth": 0 }
                    ]
                }},
                "children": []
            },
            {
                "id": "assert-greeted",
                "type": "assertion",
                "props": { "data": {
                    "output": "customer.greeted",
                    "conditions": [
                        { "id": "c2", "expression": "customer.nickname != null", "operator": "and", "depth": 0 }
                    ]
                }},
                "children": []
            }
        ]
    })
}

#[test]
fn optional_input_omission_does_not_flag_missing() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "policy",
        serde_json::from_value(optional_input_document()).unwrap(),
    );

    let greeted = ws.evaluate(&EvaluateRequest {
        policy_path: Arc::from("policy"),
        input: Variable::from(json!({ "customer": {} })),
        goals: vec![Arc::from("customer.greeted")],
        trace: false,
    });
    assert!(
        greeted.is_ok(),
        "omitting optional input customer.nickname must not raise MissingRequiredInputs; got {:?}",
        greeted.err(),
    );

    let adult = ws.evaluate(&EvaluateRequest {
        policy_path: Arc::from("policy"),
        input: Variable::from(json!({ "customer": {} })),
        goals: vec![Arc::from("customer.adult")],
        trace: false,
    });
    assert!(
        matches!(adult, Err(EvaluationError::MissingRequiredInputs { .. })),
        "omitting required input customer.age must raise MissingRequiredInputs; got {adult:?}",
    );
}

#[test]
fn dependencies_distinguishes_per_write_in_multi_output_tree() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "policy",
        serde_json::from_value(analysis_document()).unwrap(),
    );

    let total = ws.dependencies("customer.totalRevenue");
    let dep_paths: Vec<String> = total.deps.iter().map(|d| d.property.to_string()).collect();
    assert!(
        dep_paths.iter().any(|p| p.contains("customer.companies")),
        "totalRevenue should depend on customer.companies; got {dep_paths:?}",
    );
    assert!(
        !dep_paths.iter().any(|p| p == "customer.creditReport.score"),
        "totalRevenue must NOT depend on customer.creditReport.score (per-write granularity); got {dep_paths:?}",
    );

    let tier = ws.dependencies("customer.creditTier");
    let tier_paths: Vec<String> = tier.deps.iter().map(|d| d.property.to_string()).collect();
    assert!(
        tier_paths
            .iter()
            .any(|p| p == "customer.creditReport.score"),
        "creditTier should depend on customer.creditReport.score; got {tier_paths:?}",
    );

    let discount = ws.dependencies("customer.discount");
    let discount_first: Vec<String> = discount
        .deps
        .iter()
        .map(|d| d.property.to_string())
        .collect();
    assert!(
        discount_first.iter().any(|p| p == "customer.creditTier"),
        "discount should directly depend on creditTier; got {discount_first:?}",
    );
    assert!(
        discount_first.iter().any(|p| p == "customer.totalRevenue"),
        "discount should directly depend on totalRevenue; got {discount_first:?}",
    );
    let credit_node = discount
        .deps
        .iter()
        .find(|d| d.property.as_ref() == "customer.creditTier")
        .expect("creditTier child");
    let credit_grandchildren: Vec<String> = credit_node
        .deps
        .iter()
        .map(|d| d.property.to_string())
        .collect();
    assert!(
        credit_grandchildren
            .iter()
            .any(|p| p == "customer.creditReport.score"),
        "discount → creditTier → customer.creditReport.score should appear transitively; got {credit_grandchildren:?}",
    );
}

#[test]
fn input_skeleton_omits_back_references_and_terminates() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "test",
        serde_json::from_value(data_model_document()).unwrap(),
    );

    let skeleton = ws.input_skeleton(&ScopeRequest::for_policy("test"));

    let root = skeleton.as_object().expect("skeleton is an object");
    let customer = root
        .get("customer")
        .and_then(|v| v.as_object())
        .expect("customer top-level missing");

    assert_eq!(customer.get("name"), Some(&serde_json::json!("")));
    assert_eq!(customer.get("age"), Some(&serde_json::json!(0)));

    assert_eq!(
        customer.get("companies"),
        Some(&serde_json::json!([{ "id": "", "revenue": 0 }])),
    );

    assert_eq!(
        customer.get("favoriteProduct"),
        Some(&serde_json::json!(""))
    );

    let credit = customer
        .get("creditReport")
        .and_then(|v| v.as_object())
        .expect("creditReport defaults missing");
    assert_eq!(credit.get("score"), Some(&serde_json::json!(0)));
    assert!(
        !credit.contains_key("customer"),
        "creditReport must not carry a back-reference to customer in the skeleton; got {credit:#?}",
    );

    assert_eq!(
        root.get("product"),
        Some(&serde_json::json!([{ "id": "", "name": "" }])),
    );
}

fn analysis_document() -> serde_json::Value {
    let mut dm = data_model_document();
    let blocks = dm["blocks"].as_array_mut().unwrap();

    blocks.push(json!({
        "id": "ds1",
        "type": "expression",
        "props": {
            "data": json!({
                "key": "customer.totalRevenue",
                "value": "sum(map(customer.companies as c, c.revenue))"
            })
        }
    }));

    blocks.push(json!({
        "id": "f1",
        "type": "match",
        "props": {
            "data": json!({
                "key": "customer.creditTier",
                "arms": [
                    { "id": "db1", "condition": "customer.creditReport.score >= 750", "value": "\"excellent\"" },
                    { "id": "db2", "condition": "", "value": "\"good\"" }
                ]
            })
        }
    }));

    blocks.push(json!({
        "id": "dt1",
        "type": "decisionTable",
        "props": {
            "data": json!({
                "hitPolicy": "first",
                "inputs": [
                    { "id": "i1", "name": "", "field": "customer.creditTier" },
                    { "id": "i2", "name": "", "field": "customer.totalRevenue" }
                ],
                "outputs": [
                    { "id": "o1", "name": "", "field": "customer.discount" }
                ],
                "rules": [
                    { "i1": "\"excellent\"", "i2": ">= 500000", "o1": "0.2" },
                    { "i1": "\"excellent\"", "i2": "", "o1": "0.15" },
                    { "i1": "", "i2": "", "o1": "0.05" }
                ]
            })
        }
    }));

    dm
}

#[test]
fn analysis_no_errors_and_outputs_present() {
    let mut ws = PolicyWorkspace::new();
    let doc = analysis_document();
    ws.set_policy("policy", serde_json::from_value(doc).unwrap());

    let diags = ws.diagnostics("policy");
    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    for e in &errors {
        eprintln!("  [{:?}] {} @ {:?}", e.code, e.message, e.location);
    }
    assert!(
        errors.is_empty(),
        "expected no errors, got {}",
        errors.len()
    );

    let outputs = ws.outputs(&ScopeRequest::for_policy("policy"));
    let discount = outputs
        .iter()
        .find(|p| p.path.as_ref() == "customer.discount");
    assert!(discount.is_some(), "customer.discount missing from outputs");
    assert!(matches!(
        discount.unwrap().resolved_type,
        VariableType::Number
    ));

    let credit_tier_refs = ws.references(&zen_engine::policy::RenameTarget::Field {
        entity: Arc::from("customer"),
        field: Arc::from("creditTier"),
    });
    assert!(
        credit_tier_refs
            .iter()
            .any(|s| s.block_id.as_ref() == "dt1"),
        "customer.creditTier should be referenced inside dt1; got {credit_tier_refs:#?}",
    );
    let total_revenue_refs = ws.references(&zen_engine::policy::RenameTarget::Field {
        entity: Arc::from("customer"),
        field: Arc::from("totalRevenue"),
    });
    assert!(
        total_revenue_refs
            .iter()
            .any(|s| s.block_id.as_ref() == "dt1"),
        "customer.totalRevenue should be referenced inside dt1; got {total_revenue_refs:#?}",
    );
}

#[test]
fn columns_kind_shape_is_accepted() {
    let doc = json!({
        "blocks": [
            {
                "id": "dm",
                "type": "dataModel",
                "props": {
                    "data": json!({
                        "name": "customer",
                        "properties": [
                            { "id": "a", "name": "age", "type": "number", "array": false, "optional": false }
                        ]
                    })
                }
            },
            {
                "id": "dt",
                "type": "decisionTable",
                "props": {
                    "data": json!({
                        "hitPolicy": "first",
                        "columns": [
                            { "id": "i1", "field": "customer.age", "kind": "input" },
                            { "id": "o1", "field": "customer.basePrice", "kind": "output" }
                        ],
                        "rules": [
                            { "i1": ">= 65", "o1": "80"  },
                            { "i1": ">= 18", "o1": "100" },
                            { "i1": "",      "o1": "50"  }
                        ]
                    })
                }
            }
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("pricing", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("pricing")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {:?}", errors);

    let outputs = ws.outputs(&ScopeRequest::for_policy("pricing"));
    let base_price = outputs
        .iter()
        .find(|p| p.path.as_ref() == "customer.basePrice")
        .expect("customer.basePrice should be registered");
    assert!(matches!(base_price.resolved_type, VariableType::Number));

    let req = EvaluateRequest {
        policy_path: Arc::from("pricing"),
        input: Variable::from(json!({ "customer": { "age": 35 } })),
        goals: Vec::new(),
        trace: true,
    };
    let result = ws.evaluate(&req).expect("evaluate should succeed");
    let serialized: serde_json::Value = result.output.into();
    assert_eq!(
        serialized
            .get("customer")
            .and_then(|c| c.get("basePrice"))
            .and_then(|v| v.as_i64()),
        Some(100),
        "customer.basePrice should be 100 for age 35; got {:?}",
        serialized
    );
    let trace = result.trace.expect("trace should be populated");
    assert_eq!(trace.executions.len(), 1);
}

#[test]
fn invalid_input_cell_errors() {
    let mut dm = data_model_document();
    let blocks = dm["blocks"].as_array_mut().unwrap();
    blocks.push(json!({
        "id": "dt-bad",
        "type": "decisionTable",
        "props": {
            "data": json!({
                "hitPolicy": "first",
                "inputs": [ { "id": "i1", "name": "", "field": "customer.age" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.discount" } ],
                "rules": [
                    { "i1": "tru", "o1": "0.1" },
                    { "i1": "",    "o1": "0.05" }
                ]
            })
        }
    }));

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("test", serde_json::from_value(dm).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("test")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(!errors.is_empty(), "expected errors for invalid cell 'tru'");
}

#[test]
fn multi_policy_with_import() {
    let shared_schema = json!({
        "name": "customer",
        "properties": [
            { "id": "a", "name": "age", "type": "number", "array": false, "optional": false }
        ]
    });

    let pricing = json!({
        "blocks": [
            { "id": "dm-pricing", "type": "dataModel", "props": { "data": shared_schema.clone() } },
            {
                "id": "dt-pricing",
                "type": "decisionTable",
                "props": {
                    "data": json!({
                        "hitPolicy": "first",
                        "columns": [
                            { "id": "i1", "field": "customer.age", "kind": "input" },
                            { "id": "o1", "field": "customer.basePrice", "kind": "output" }
                        ],
                        "rules": [
                            { "i1": ">= 65", "o1": "80" },
                            { "i1": ">= 18", "o1": "100" },
                            { "i1": "",      "o1": "50" }
                        ]
                    })
                }
            }
        ]
    });

    let order = json!({
        "imports": ["pricing"],
        "blocks": [
            { "id": "dm-order", "type": "dataModel", "props": { "data": shared_schema.clone() } },
            {
                "id": "a-order",
                "type": "assertion",
                "props": {
                    "data": json!({
                        "output": "customer.approved",
                        "conditions": [
                            { "id": "c1", "expression": "customer.basePrice > 0", "operator": "and", "depth": 0 }
                        ]
                    })
                }
            }
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("pricing", serde_json::from_value(pricing).unwrap());
    ws.set_policy("order", serde_json::from_value(order).unwrap());

    for path in ["pricing", "order"] {
        let errors: Vec<_> = ws
            .diagnostics(path)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "[{path}] expected no errors, got {:?}",
            errors
        );
    }

    let outputs = ws.outputs(&ScopeRequest::for_policy("order"));
    let bp = outputs
        .iter()
        .find(|p| p.path.as_ref() == "customer.basePrice")
        .expect("customer.basePrice should be visible via import");
    let writer = bp.written_by.as_ref().expect("writer present");
    assert_eq!(writer.policy_path.as_ref(), "pricing");
    assert_eq!(writer.block_id.as_ref(), "dt-pricing");

    let req = EvaluateRequest {
        policy_path: Arc::from("order"),
        input: Variable::from(json!({ "customer": { "age": 35 } })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let out: serde_json::Value = result.output.into();
    assert_eq!(
        out.get("customer")
            .and_then(|c| c.get("basePrice"))
            .and_then(|v| v.as_i64()),
        Some(100)
    );
    assert_eq!(
        out.get("customer")
            .and_then(|c| c.get("approved"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn missing_import_diagnostic_and_evaluate_error() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "main",
        serde_json::from_value(json!({
            "imports": ["does-not-exist"],
            "blocks": [ expr("e1", "x", "1 + 1") ]
        }))
        .unwrap(),
    );

    let errs = errors(&ws, "main");
    assert!(
        errs.iter().any(|e| e.contains("ImportNotFound")),
        "expected ImportNotFound diagnostic, got {errs:?}"
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("main"),
        input: Variable::from(json!({})),
        goals: Vec::new(),
        trace: false,
    };
    let err = ws
        .evaluate(&req)
        .expect_err("evaluate must fail when an import is missing");
    assert!(
        matches!(
            &err,
            EvaluationError::ImportNotFound { policy_path, import }
                if policy_path.as_ref() == "main" && import.as_ref() == "does-not-exist"
        ),
        "expected ImportNotFound, got {err:?}"
    );

    let err = ws
        .enhance_trace(&req)
        .expect_err("enhance_trace must fail when an import is missing");
    assert!(matches!(&err, EvaluationError::ImportNotFound { .. }));
}

#[test]
fn missing_transitive_import_fails_evaluate() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "mid",
        serde_json::from_value(json!({
            "imports": ["does-not-exist"],
            "blocks": [ expr("e1", "y", "2") ]
        }))
        .unwrap(),
    );
    ws.set_policy(
        "main",
        serde_json::from_value(json!({
            "imports": ["mid"],
            "blocks": [ expr("e2", "x", "1") ]
        }))
        .unwrap(),
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("main"),
        input: Variable::from(json!({})),
        goals: Vec::new(),
        trace: false,
    };
    let err = ws
        .evaluate(&req)
        .expect_err("evaluate must fail when a transitive import is missing");
    assert!(
        matches!(
            &err,
            EvaluationError::ImportNotFound { policy_path, import }
                if policy_path.as_ref() == "mid" && import.as_ref() == "does-not-exist"
        ),
        "expected ImportNotFound on 'mid', got {err:?}"
    );
}

fn closure_doc() -> serde_json::Value {
    json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "customer.totalRevenue",
                "value": "sum(map(customer.companies as c, c.revenue))"
            }) }}
        ]
    })
}

#[test]
fn rename_follows_both_direct_refs_and_closure_aliases() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(closure_doc()).unwrap());

    let edits = ws.rename(
        &zen_engine::policy::RenameTarget::Entity {
            name: Arc::from("customer"),
        },
        "buyer",
    );
    assert!(
        any_rewritten_contains(&edits, "buyer.companies"),
        "entity rename should rewrite `customer.companies` → `buyer.companies` in the closure",
    );
    assert!(
        any_rewritten_contains(&edits, "buyer.totalRevenue"),
        "entity rename should rewrite the write key `customer.totalRevenue` → `buyer.totalRevenue`",
    );

    let edits = ws.rename(
        &zen_engine::policy::RenameTarget::Field {
            entity: Arc::from("company"),
            field: Arc::from("revenue"),
        },
        "income",
    );
    assert!(
        any_touches_block(&edits, "dm-company"),
        "DataModel declaration of company.revenue should be renamed",
    );
    let tree_json = rewritten_block_json(&edits, "tree")
        .expect("rename should rewrite the tree block holding the closure body");
    assert!(
        tree_json.contains("c.income"),
        "aliased access `c.revenue` should become `c.income`; got {tree_json}",
    );
}

#[test]
fn nested_iteration_uses_owner_back_reference() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "id", "type": "string", "array": false, "optional": false },
                    { "id": "p3", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "company.isHighestRevenue",
                "value": "max(map(company.customer.companies as c, c.revenue)) == company.revenue"
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {errors:#?}",);

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({
            "customer": {
                "companies": [
                    { "id": "c1", "revenue": 500 },
                    { "id": "c2", "revenue": 1000 }
                ]
            }
        })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate should succeed");
    let serialized: serde_json::Value = result.output.into();
    let companies = serialized
        .get("customer")
        .and_then(|c| c.get("companies"))
        .and_then(|v| v.as_array())
        .expect("customer.companies present");

    let flags: Vec<bool> = companies
        .iter()
        .map(|c| {
            c.get("isHighestRevenue")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(flags, vec![false, true], "c2 has the highest revenue");
}

#[test]
fn completions_follow_cyclic_owner_back_reference_deeply() {
    let expr = "company.customer.companies[0].customer.companies[0].";
    let cursor_pos = expr.len() as u32;

    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "id", "type": "string", "array": false, "optional": false },
                    { "id": "p3", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "company.tag", "value": expr
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let completions = ws.completions(&Cursor {
        policy_path: Arc::from("p"),
        block_id: Arc::from("tree"),
        pos: cursor_pos,
        target: CursorTarget::Expression {
            id: Arc::from("s1"),
        },
    });
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        !labels.is_empty(),
        "expected non-empty completions at deep back-ref"
    );
    for expected in ["id", "revenue", "customer"] {
        assert!(
            labels.iter().any(|l| *l == expected),
            "expected completion '{expected}' in {labels:?}",
        );
    }
}

#[test]
fn rename_entity_from_decision_table_head() {
    let doc = json!({
        "blocks": [
            { "id": "dm1", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dt1", "type": "decisionTable", "props": { "data": json!({
                "hitPolicy": "first",
                "inputs": [ { "id": "i1", "name": "", "field": "customer.age" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.price" } ],
                "rules": [ { "i1": ">= 18", "o1": "100" } ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let cursor = Cursor {
        policy_path: Arc::from("p"),
        block_id: Arc::from("dt1"),
        pos: 0,
        target: CursorTarget::DecisionTableHead {
            col: Arc::from("i1"),
        },
    };

    let prep = ws.prepare_rename(&cursor).expect("prepare_rename");
    let edits = ws.rename(&prep.target, "buyer");
    assert!(!edits.is_empty(), "rename produced no edits");
}

#[test]
fn entities_includes_imported_policy_fields() {
    let kyc = json!({
        "blocks": [
            { "id": "dm-cust", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false },
                    { "id": "p2", "name": "country", "type": "string", "array": false, "optional": false },
                    { "id": "p3", "name": "kycVerified", "type": "boolean", "array": false, "optional": false }
                ]
            }) }}
        ]
    });
    let order = json!({
        "imports": ["underwriting/kyc"],
        "blocks": [
            { "id": "ord-a", "type": "assertion", "props": { "data": json!({
                "output": "customer.orderApproved",
                "conditions": [
                    { "id": "c1", "expression": "customer.kycVerified == true", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("underwriting/kyc", serde_json::from_value(kyc).unwrap());
    ws.set_policy("risk/order", serde_json::from_value(order).unwrap());

    let diagnostics = ws.diagnostics("risk/order");
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "expected no errors compiling risk/order; got {errors:#?}"
    );

    let entities = ws.entities(&ScopeRequest::for_policy("risk/order"));
    let customer = entities
        .iter()
        .find(|e| e.name.as_ref() == "customer")
        .unwrap_or_else(|| panic!("customer entity missing from entities(); got {entities:#?}"));

    let field_names: Vec<&str> = customer.fields.iter().map(|f| f.name.as_ref()).collect();
    eprintln!("entities()['customer'] fields: {field_names:?}");
    for required in ["age", "country", "kycVerified", "orderApproved"] {
        assert!(
            field_names.iter().any(|n| *n == required),
            "expected field `{required}` in customer entity; got {field_names:?}"
        );
    }
}

#[test]
fn policy_merge_evaluates_correctly() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "id", "type": "string", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "customer.merged",
                "value": "merge([{a: 1, b: 2}, {b: 99, c: 3}])"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "id": "x" } })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let out: serde_json::Value = result.output.into();
    let merged = out
        .get("customer")
        .and_then(|c| c.get("merged"))
        .expect("customer.merged missing");
    assert_eq!(merged.get("a").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(merged.get("b").and_then(|v| v.as_i64()), Some(99));
    assert_eq!(merged.get("c").and_then(|v| v.as_i64()), Some(3));
}

#[test]
fn rename_field_on_array_index_without_back_ref() {
    let expr = "customer.companies[0].revenue";
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "customer.topRevenue", "value": expr
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let edits = ws.rename(
        &zen_engine::policy::RenameTarget::Field {
            entity: Arc::from("company"),
            field: Arc::from("revenue"),
        },
        "income",
    );

    let tree_json = rewritten_block_json(&edits, "tree")
        .unwrap_or_else(|| panic!("rename should rewrite the tree block; got {edits:#?}"));
    assert!(
        tree_json.contains("customer.companies[0].income"),
        "rename should rewrite trailing .revenue → .income; got {tree_json}",
    );
    assert!(
        !tree_json.contains("customer.companies[0].revenue"),
        "old `.revenue` should be gone; got {tree_json}",
    );
}

#[test]
fn rename_field_through_indexed_back_reference_chain() {
    let expr = "company.customer.companies[0].customer.creditTier";
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false },
                    { "id": "p2", "name": "creditTier", "type": "string", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p3", "name": "id", "type": "string", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "company.hello", "value": expr
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {errors:#?}");

    let edits = ws.rename(
        &zen_engine::policy::RenameTarget::Field {
            entity: Arc::from("customer"),
            field: Arc::from("creditTier"),
        },
        "creditTiera",
    );

    assert!(
        any_touches_block(&edits, "dm-customer"),
        "rename should update customer.creditTier in dm-customer; got edits: {edits:#?}",
    );

    let tree_json = rewritten_block_json(&edits, "tree")
        .unwrap_or_else(|| panic!("rename should rewrite the tree block; got edits: {edits:#?}"));
    let expected = format!("company.customer.companies[0].customer.{}", "creditTiera");
    assert!(
        tree_json.contains(&expected),
        "expected rewritten `{expected}` in tree block; got {tree_json}",
    );
}

#[test]
fn global_scalar_appears_at_json_top_level_and_evaluates() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "taxRate", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "subtotal", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "customer.taxed",
                "conditions": [
                    { "id": "c1", "expression": "customer.subtotal * (1 + taxRate) > 0", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {errors:#?}");

    let inputs = ws.inputs(&ScopeRequest::for_policy("p"));
    let tax = inputs
        .iter()
        .find(|p| p.path.as_ref() == "taxRate")
        .expect("taxRate should be a top-level input");
    assert!(matches!(tax.resolved_type, VariableType::Number));

    let entities = ws.entities(&ScopeRequest::for_policy("p"));
    assert!(
        entities.iter().all(|e| e.name.as_ref() != "platform"),
        "platform (global DM name) must not surface as an entity",
    );

    let skeleton = ws.input_skeleton(&ScopeRequest::for_policy("p"));
    assert_eq!(
        skeleton.as_object().and_then(|o| o.get("taxRate")),
        Some(&serde_json::json!(0)),
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({
            "taxRate": 0.2,
            "customer": { "subtotal": 100 }
        })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate should succeed");
    let out: serde_json::Value = result.output.into();
    assert_eq!(
        out.get("customer")
            .and_then(|c| c.get("taxed"))
            .and_then(|v| v.as_bool()),
        Some(true),
        "customer.taxed should compute from global taxRate; got {out:?}",
    );
}

#[test]
fn global_property_name_collides_with_entity_name() {
    let doc = json!({
        "blocks": [
            { "id": "dm-entity", "type": "dataModel", "props": { "data": json!({
                "name": "tenant",
                "properties": [
                    { "id": "p1", "name": "id", "type": "string", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-global", "type": "dataModel", "props": { "data": json!({
                "name": "settings",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "tenant", "type": "string", "array": false, "optional": false }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| format!("{:?}", d.code) == "DataModelCollision"),
        "expected DataModelCollision; got {errors:#?}",
    );
}

#[test]
fn block_writing_entity_and_global_is_mixed_scope() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "settings",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "lastSeenAge", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "decisionTable", "props": { "data": json!({
                "hitPolicy": "first",
                "inputs": [],
                "outputs": [
                    { "id": "s1", "name": "", "field": "customer.adult" },
                    { "id": "s2", "name": "", "field": "lastSeenAge" }
                ],
                "rules": [
                    { "_id": "r1", "s1": "customer.age >= 18", "s2": "customer.age" }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| format!("{:?}", d.code) == "MixedScope"),
        "expected MixedScope when block writes both entity and global; got {errors:#?}",
    );
}

#[test]
fn global_relationship_iterates_top_level_array() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "tenants", "type": "relationship", "target": "tenant", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-tenant", "type": "dataModel", "props": { "data": json!({
                "name": "tenant",
                "properties": [
                    { "id": "p1", "name": "id", "type": "string", "array": false, "optional": false },
                    { "id": "p2", "name": "seats", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "tenant.large", "value": "tenant.seats >= 100"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {errors:#?}");

    let inputs = ws.inputs(&ScopeRequest::for_policy("p"));
    let tenants = inputs
        .iter()
        .find(|p| p.path.as_ref() == "tenants")
        .expect("tenants should be a top-level array input");
    assert!(matches!(tenants.resolved_type, VariableType::Array(_)));
    assert!(
        !inputs.iter().any(|p| p.path.as_ref() == "tenant"),
        "tenant pool should not surface separately; got inputs {:?}",
        inputs.iter().map(|i| i.path.as_ref()).collect::<Vec<_>>(),
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({
            "tenants": [
                { "id": "t1", "seats": 50 },
                { "id": "t2", "seats": 250 }
            ]
        })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate should succeed");
    let out: serde_json::Value = result.output.into();
    let tenants_out = out
        .get("tenants")
        .and_then(|v| v.as_array())
        .expect("tenants present");
    let flags: Vec<bool> = tenants_out
        .iter()
        .map(|t| t.get("large").and_then(|v| v.as_bool()).unwrap_or(false))
        .collect();
    assert_eq!(flags, vec![false, true], "second tenant has >=100 seats");
}

#[test]
fn writing_to_a_global_input_is_rejected() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "taxRate", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "taxRate",
                "conditions": [
                    { "id": "c1", "expression": "true", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| format!("{:?}", d.code) == "InputOverride"),
        "expected InputOverride for write to global input; got {errors:#?}",
    );
}

#[test]
fn cross_policy_global_collision_with_mismatched_shape() {
    let policy_a = json!({
        "blocks": [
            { "id": "dm-a", "type": "dataModel", "props": { "data": json!({
                "name": "settings",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "region", "type": "string", "array": false, "optional": false }
                ]
            }) }}
        ]
    });
    let policy_b = json!({
        "imports": ["a"],
        "blocks": [
            { "id": "dm-b", "type": "dataModel", "props": { "data": json!({
                "name": "settings",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "region", "type": "number", "array": false, "optional": false }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("a", serde_json::from_value(policy_a).unwrap());
    ws.set_policy("b", serde_json::from_value(policy_b).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("b")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| format!("{:?}", d.code) == "DataModelCollision"),
        "expected DataModelCollision for mismatched global shapes; got {errors:#?}",
    );
}

#[test]
fn rename_global_scalar_rewrites_reads_writes_and_declaration() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "taxRate", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "subtotal", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "customer.taxed",
                "value": "customer.subtotal * (1 + taxRate)"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let edits = ws.rename(
        &zen_engine::policy::RenameTarget::Global {
            name: Arc::from("taxRate"),
        },
        "vatRate",
    );

    assert!(
        any_touches_block(&edits, "dm-globals"),
        "rename must touch the global DM declaration; got {edits:#?}",
    );

    let tree_json = rewritten_block_json(&edits, "tree")
        .unwrap_or_else(|| panic!("rename must rewrite the tree block; got {edits:#?}"));
    assert!(
        tree_json.contains("vatRate"),
        "tree should contain the rewritten identifier; got {tree_json}",
    );
    assert!(
        !tree_json.contains("taxRate"),
        "old identifier should be gone from tree; got {tree_json}",
    );
}

#[test]
fn rename_global_relationship_chains_into_target_entity() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "tenants", "type": "relationship", "target": "tenant", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-tenant", "type": "dataModel", "props": { "data": json!({
                "name": "tenant",
                "properties": [
                    { "id": "p1", "name": "id", "type": "string", "array": false, "optional": false },
                    { "id": "p2", "name": "seats", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "tenant.large",
                "value": "tenant.seats >= 100"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let edits = ws.rename(
        &zen_engine::policy::RenameTarget::Entity {
            name: Arc::from("tenant"),
        },
        "client",
    );

    let globals_json = rewritten_block_json(&edits, "dm-globals")
        .unwrap_or_else(|| panic!("rename must rewrite the global DM; got {edits:#?}"));
    assert!(
        globals_json.contains("\"client\""),
        "global Relationship target should be rewritten to `client`; got {globals_json}",
    );

    let tree_json = rewritten_block_json(&edits, "tree")
        .unwrap_or_else(|| panic!("rename must rewrite the tree block; got {edits:#?}"));
    assert!(
        tree_json.contains("client.seats"),
        "expression read should be rewritten; got {tree_json}",
    );
    assert!(
        tree_json.contains("client.large"),
        "write key should be rewritten; got {tree_json}",
    );
}

#[test]
fn enhance_trace_populates_dt_input_pass_bitmask() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false },
                    { "id": "p2", "name": "country", "type": "string", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dt", "type": "decisionTable", "props": { "data": json!({
                "hitPolicy": "collect",
                "inputs": [
                    { "id": "i1", "name": "", "field": "customer.age" },
                    { "id": "i2", "name": "", "field": "customer.country" }
                ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.tier" } ],
                "rules": [
                    { "i1": ">= 18", "i2": "\"US\"", "o1": "\"adult-us\"" },
                    { "i1": ">= 18", "i2": "\"CA\"", "o1": "\"adult-ca\"" },
                    { "i1": "< 18",  "i2": "",      "o1": "\"minor\""    }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "age": 25, "country": "US" } })),
        goals: Vec::new(),
        trace: true,
    };
    let result = ws.enhance_trace(&req).expect("enhance succeeded");
    let trace = result.trace.expect("trace populated");
    assert_eq!(trace.engine_version.as_ref(), env!("CARGO_PKG_VERSION"));

    let dt_exec = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt")
        .expect("dt execution present");

    let serde_json::Value::Object(trace_obj) = serde_json::to_value(&dt_exec.trace).unwrap() else {
        panic!("dt trace not an object");
    };
    let extras = trace_obj
        .get("extras")
        .and_then(|v| v.as_object())
        .expect("extras present under enhance_trace");
    let input_pass_b64 = extras
        .get("inputPass")
        .and_then(|v| v.as_str())
        .expect("inputPass present");

    let bytes = base64_decode(input_pass_b64);
    let cols = 2usize;
    let bytes_per_row = cols.div_ceil(8);
    let bit = |row: usize, col: usize| -> bool {
        let byte = row * bytes_per_row + col / 8;
        (bytes[byte] >> (col % 8)) & 1 == 1
    };
    assert!(bit(0, 0), "row 0 col 0 (age >= 18) should pass for age=25");
    assert!(bit(0, 1), "row 0 col 1 (\"US\") should pass for US");
    assert!(bit(1, 0), "row 1 col 0 (age >= 18) should pass for age=25");
    assert!(!bit(1, 1), "row 1 col 1 (\"CA\") should fail for US");
    assert!(!bit(2, 0), "row 2 col 0 (age < 18) should fail for age=25");
    assert!(bit(2, 1), "row 2 col 1 (wildcard) passes");
}

fn base64_decode(s: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).unwrap()
}

#[test]
fn enhance_trace_operand_values_are_flat_path_keyed() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "totalRevenue", "type": "number", "array": false, "optional": false },
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false },
                    { "id": "p3", "name": "threshold", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dt", "type": "decisionTable", "props": { "data": json!({
                "hitPolicy": "first",
                "inputs": [
                    { "id": "i1", "name": "", "field": "customer.totalRevenue" },
                    { "id": "i2", "name": "", "field": "" }
                ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.discount" } ],
                "rules": [
                    { "_id": "r1", "i1": "$ > customer.revenue", "i2": "customer.threshold > customer.revenue", "o1": "0.1" }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(
            json!({ "customer": { "totalRevenue": 700000, "revenue": 500000, "threshold": 100 } }),
        ),
        goals: Vec::new(),
        trace: true,
    };
    let result = ws.enhance_trace(&req).expect("enhance succeeded");
    let dt = result
        .trace
        .expect("trace populated")
        .executions
        .into_iter()
        .find(|e| e.block_id.as_ref() == "dt")
        .expect("dt execution present");

    let ops: std::collections::BTreeMap<String, serde_json::Value> = dt
        .operand_values
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone().into()))
        .collect();

    let expected: std::collections::BTreeMap<String, serde_json::Value> = [
        ("customer.totalRevenue".to_string(), json!(700000)),
        ("customer.revenue".to_string(), json!(500000)),
        ("customer.threshold".to_string(), json!(100)),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        ops, expected,
        "operand values must be a flat property-path → value map (header read + in-cell reads, one entry per path, no expression-id buckets or spans)",
    );

    let json = serde_json::to_value(&dt).unwrap();
    let serialized = serde_json::to_string(&json).unwrap();
    assert!(
        json["operandValues"]["customer.revenue"] == json!(500000),
        "serialized operandValues must be keyed by property path; got {serialized}",
    );
    assert!(
        !serialized.contains("\"span\""),
        "operand values must no longer carry spans; got {serialized}",
    );
    assert!(
        !serialized.contains("\"i1\"")
            && !serialized.contains("\"i2\"")
            && !serialized.contains("\"o1\""),
        "operand values must not be keyed by expression/column id; got {serialized}",
    );
}

#[test]
fn enhance_trace_operand_values_per_instance() {
    let doc: serde_json::Value = serde_json::from_str(
        &fs::read_to_string("tests/data/policy/fixtures/per_instance.json").unwrap(),
    )
    .unwrap();
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "name": "Alice", "companies": [
            { "name": "Acme", "revenue": 300000 },
            { "name": "Smol", "revenue": 50000 }
        ]}})),
        goals: Vec::new(),
        trace: true,
    };
    let result = ws.enhance_trace(&req).expect("enhance succeeded");
    let execs: Vec<_> = result
        .trace
        .expect("trace populated")
        .executions
        .into_iter()
        .filter(|e| e.block_id.as_ref() == "company-risk")
        .collect();
    assert_eq!(execs.len(), 2, "one execution per company instance");

    let value_for = |instance: &str| -> serde_json::Value {
        execs
            .iter()
            .find(|e| e.instance_path.as_deref() == Some(instance))
            .and_then(|e| e.operand_values.get("company.revenue").cloned())
            .map(Into::into)
            .unwrap_or(serde_json::Value::Null)
    };
    assert_eq!(
        value_for("customer.companies.0"),
        json!(300000),
        "operand value resolves against the per-instance scope",
    );
    assert_eq!(value_for("customer.companies.1"), json!(50000));
}

#[test]
fn enhance_trace_populates_match_arm_results() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "score", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "match", "props": { "data": json!({
                "key": "customer.tier",
                "arms": [
                    { "id": "b1", "condition": "customer.score >= 750", "value": "\"gold\"" },
                    { "id": "b2", "condition": "customer.score >= 500", "value": "\"silver\"" },
                    { "id": "b3", "condition": "", "value": "\"bronze\"" }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "score": 600 } })),
        goals: Vec::new(),
        trace: true,
    };
    let result = ws.enhance_trace(&req).expect("enhance succeeded");
    let trace = result.trace.expect("trace populated");

    let tree_exec = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "tree")
        .expect("match execution present");
    let trace_obj = serde_json::to_value(&tree_exec.trace).unwrap();
    let arms = trace_obj
        .get("arms")
        .and_then(|v| v.as_array())
        .expect("arms present");
    let result_for = |id: &str| {
        arms.iter()
            .find(|a| a.get("id").and_then(|v| v.as_str()) == Some(id))
            .and_then(|a| a.get("result").and_then(|v| v.as_bool()))
    };

    assert_eq!(
        trace_obj.get("matchedArm").and_then(|v| v.as_str()),
        Some("b2")
    );
    assert_eq!(result_for("b1"), Some(false));
    assert_eq!(result_for("b2"), Some(true));
    assert_eq!(
        result_for("b3"),
        Some(true),
        "b3 (default arm) must be evaluated under extras so the UI can color it; got {arms:?}",
    );
}

#[test]
fn default_trace_omits_extras() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dt", "type": "decisionTable", "props": { "data": json!({
                "hitPolicy": "first",
                "inputs": [ { "id": "i1", "name": "", "field": "customer.age" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.tier" } ],
                "rules": [ { "i1": ">= 18", "o1": "\"adult\"" } ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "age": 25 } })),
        goals: Vec::new(),
        trace: true,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let trace = result.trace.expect("trace populated");
    assert_eq!(trace.engine_version.as_ref(), env!("CARGO_PKG_VERSION"));
    let dt_exec = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt")
        .unwrap();
    let trace_json = serde_json::to_string(&dt_exec.trace).unwrap();
    assert!(
        !trace_json.contains("extras"),
        "default trace should omit extras; got {trace_json}",
    );
}

#[test]
fn trace_block_execution_tags_imported_policy_only() {
    let shared_schema = json!({
        "name": "customer",
        "properties": [
            { "id": "a", "name": "age", "type": "number", "array": false, "optional": false }
        ]
    });
    let pricing = json!({
        "blocks": [
            { "id": "dm-pricing", "type": "dataModel", "props": { "data": shared_schema.clone() } },
            {
                "id": "dt-pricing",
                "type": "decisionTable",
                "props": {
                    "data": json!({
                        "hitPolicy": "first",
                        "columns": [
                            { "id": "i1", "field": "customer.age", "kind": "input" },
                            { "id": "o1", "field": "customer.basePrice", "kind": "output" }
                        ],
                        "rules": [
                            { "i1": ">= 18", "o1": "100" },
                            { "i1": "",      "o1": "50" }
                        ]
                    })
                }
            }
        ]
    });
    let order = json!({
        "imports": ["pricing"],
        "blocks": [
            { "id": "dm-order", "type": "dataModel", "props": { "data": shared_schema.clone() } },
            {
                "id": "a-order",
                "type": "assertion",
                "props": {
                    "data": json!({
                        "output": "customer.approved",
                        "conditions": [
                            { "id": "c1", "expression": "customer.basePrice > 0", "operator": "and", "depth": 0 }
                        ]
                    })
                }
            }
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("pricing", serde_json::from_value(pricing).unwrap());
    ws.set_policy("order", serde_json::from_value(order).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("order"),
        input: Variable::from(json!({ "customer": { "age": 35 } })),
        goals: Vec::new(),
        trace: true,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let trace = result.trace.expect("trace populated");

    let pricing_exec = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt-pricing")
        .expect("dt-pricing execution present");
    assert_eq!(
        pricing_exec.policy_path.as_deref().map(|p| p.as_ref()),
        Some("pricing"),
        "imported block must carry its owning policy_path",
    );

    let order_exec = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "a-order")
        .expect("a-order execution present");
    assert!(
        order_exec.policy_path.is_none(),
        "local block must omit policy_path (saves wire bytes); got {:?}",
        order_exec.policy_path,
    );

    let order_json = serde_json::to_string(order_exec).unwrap();
    assert!(
        !order_json.contains("policyPath"),
        "local execution must not serialize policyPath; got {order_json}",
    );
    let pricing_json = serde_json::to_string(pricing_exec).unwrap();
    assert!(
        pricing_json.contains("\"policyPath\":\"pricing\""),
        "imported execution must serialize policyPath; got {pricing_json}",
    );
}

#[test]
fn unreachable_read_of_iterated_entity_from_singleton_block() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "customer.flagged",
                "conditions": [
                    { "id": "c1", "expression": "company.revenue > 0", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| format!("{:?}", d.code) == "UnreachableEntityRead"),
        "expected UnreachableEntityRead for direct iterated read; got {errors:#?}",
    );
}

#[test]
fn unreachable_read_of_iterated_entity_from_global_block() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "globalFlag",
                "conditions": [
                    { "id": "c1", "expression": "company.revenue > 0", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| format!("{:?}", d.code) == "UnreachableEntityRead"),
        "expected UnreachableEntityRead from globals-writing block; got {errors:#?}",
    );
}

#[test]
fn iterated_read_via_closure_aggregation_is_reachable() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "tree", "type": "expression", "props": { "data": json!({
                "key": "customer.totalRevenue",
                "value": "sum(map(customer.companies as c, c.revenue))"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "aggregation should be reachable; got {errors:#?}"
    );
}

#[test]
fn iterated_block_reads_own_entity_fields() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "company.large",
                "conditions": [
                    { "id": "c1", "expression": "company.revenue >= 1000", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "iterated block reading its own entity must be reachable; got {errors:#?}",
    );
}

#[test]
fn closure_alias_sees_computed_field_on_iterated_entity() {
    let doc = json!({
        "blocks": [
            { "id": "dm-item", "type": "dataModel", "props": { "data": json!({
                "name": "item",
                "properties": [
                    { "id": "p1", "name": "price", "type": "number", "array": false, "optional": false },
                    { "id": "p2", "name": "quantity", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-order", "type": "dataModel", "props": { "data": json!({
                "name": "order",
                "properties": [
                    { "id": "p3", "name": "items", "type": "relationship", "target": "item", "array": true, "optional": false }
                ]
            }) }},
            { "id": "per-item", "type": "expression", "props": { "data": json!({
                "key": "item.subtotal",
                "value": "item.price * item.quantity"
            }) }},
            { "id": "per-order", "type": "expression", "props": { "data": json!({
                "key": "order.total",
                "value": "sum(map(order.items as i, i.subtotal))"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "closure alias on iterated entity must see computed fields; got {errors:#?}",
    );
}

#[test]
fn enum_field_type_checks_and_validates() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "status", "type": "string", "enum": ["active", "inactive", "pending"], "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "customer.live",
                "conditions": [
                    { "id": "c1", "expression": "customer.status == \"active\"", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "enum field should type-check; got {errors:#?}"
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "status": "active" } })),
        goals: Vec::new(),
        trace: false,
    };
    let ok = ws.evaluate(&req).expect("valid enum value should evaluate");
    let out: serde_json::Value = ok.output.into();
    assert_eq!(
        out.get("customer")
            .and_then(|c| c.get("live"))
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    let bad_req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "status": "unknown" } })),
        goals: Vec::new(),
        trace: false,
    };
    let err = ws
        .evaluate(&bad_req)
        .expect_err("unknown enum value should be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("status") && msg.contains("active"),
        "error should reference field + allowed values; got {msg}"
    );
}

#[test]
fn enum_with_duplicate_values_raises_diagnostic() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "status", "type": "string", "enum": ["active", "inactive", "active"], "array": false, "optional": false }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    let dup = errors
        .iter()
        .find(|d| format!("{:?}", d.code) == "DuplicateEnumValue")
        .expect("expected DuplicateEnumValue diagnostic");
    assert!(
        dup.message.contains("'active'"),
        "diagnostic should name the duplicate value; got {dup:?}",
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "status": "active" } })),
        goals: Vec::new(),
        trace: false,
    };
    ws.evaluate(&req)
        .expect("dedup'd enum still validates the surviving values");
}

#[test]
fn enum_field_unknown_value_in_expression_is_type_mismatch() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "status", "type": "string", "enum": ["active", "inactive"], "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "customer.live",
                "conditions": [
                    { "id": "c1", "expression": "customer.status == \"unknown\"", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        !errors.is_empty(),
        "comparing an enum field to a literal not in the enum should produce a type-check error",
    );
}

#[test]
fn globals_query_returns_schema_and_computed_entries() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "taxRate",  "type": "number", "array": false, "optional": false },
                    { "id": "g2", "name": "currency", "type": "string", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "isAdult",
                "conditions": [
                    { "id": "c1", "expression": "customer.age >= 18", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {errors:#?}");

    let globals = ws.globals(&ScopeRequest::for_policy("p"));
    let names: Vec<&str> = globals.iter().map(|g| g.name.as_ref()).collect();
    assert_eq!(names, vec!["currency", "isAdult", "taxRate"]);

    let tax = globals
        .iter()
        .find(|g| g.name.as_ref() == "taxRate")
        .unwrap();
    assert!(matches!(tax.resolved_type, VariableType::Number));
    assert!(matches!(
        tax.origin,
        zen_engine::policy::FieldOrigin::Schema { .. }
    ));

    let is_adult = globals
        .iter()
        .find(|g| g.name.as_ref() == "isAdult")
        .unwrap();
    assert!(matches!(is_adult.resolved_type, VariableType::Bool));
    assert!(matches!(
        is_adult.origin,
        zen_engine::policy::FieldOrigin::Computed { .. }
    ));

    let entities = ws.entities(&ScopeRequest::for_policy("p"));
    assert!(
        entities.iter().all(|e| e.name.as_ref() != "platform"),
        "platform (global DM name) must not appear in entities()",
    );
    assert!(
        entities.iter().all(|e| e.name.as_ref() != "isAdult"),
        "computed top-level path must not appear in entities()",
    );
}

#[test]
fn global_data_model_name_can_be_empty() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "taxRate", "type": "number", "array": false, "optional": false }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "global DM with empty name should be valid; got {errors:#?}",
    );

    let inputs = ws.inputs(&ScopeRequest::for_policy("p"));
    assert!(
        inputs.iter().any(|p| p.path.as_ref() == "taxRate"),
        "taxRate should still surface as a top-level input; got {inputs:?}",
    );
}

#[test]
fn assertion_writes_to_undeclared_global_top_level_path() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "isAdult",
                "conditions": [
                    { "id": "c1", "expression": "customer.age >= 18", "operator": "and", "depth": 0 }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "expected no errors, got {errors:#?}");

    let outputs = ws.outputs(&ScopeRequest::for_policy("p"));
    let is_adult = outputs
        .iter()
        .find(|p| p.path.as_ref() == "isAdult")
        .expect("computed top-level path should surface in outputs()");
    assert!(matches!(is_adult.resolved_type, VariableType::Bool));

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "age": 25 } })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let out: serde_json::Value = result.output.into();
    assert_eq!(
        out.get("isAdult").and_then(|v| v.as_bool()),
        Some(true),
        "isAdult must land at the JSON root; got {out:?}",
    );
}

#[test]
fn global_relationship_to_unknown_entity_raises_diagnostic() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "tenants", "type": "relationship", "target": "tenant", "array": true, "optional": false }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| format!("{:?}", d.code) == "UnknownDataModelTarget"),
        "expected UnknownDataModelTarget from a global with no declared entity; got {errors:#?}",
    );
}

#[test]
fn input_validation_rejects_wrong_type_for_global() {
    let doc = json!({
        "blocks": [
            { "id": "dm-globals", "type": "dataModel", "props": { "data": json!({
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "taxRate", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "id", "type": "string", "array": false, "optional": false }
                ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({
            "taxRate": "not a number",
            "customer": { "id": "c1" }
        })),
        goals: Vec::new(),
        trace: false,
    };
    let err = ws
        .evaluate(&req)
        .expect_err("validator should reject string for numeric global");
    assert!(
        format!("{err:?}").contains("taxRate"),
        "error should reference taxRate; got {err:?}",
    );
}

#[test]
fn expression_diagnostics_distinguish_key_from_value() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(json!({
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "s1", "type": "expression", "props": { "data": { "key": "customer.bad name", "value": "customer.age" } } },
                { "id": "s2", "type": "expression", "props": { "data": { "key": "customer.result", "value": "nonexistentVar + 1" } } }
            ]
        }))
        .unwrap(),
    );

    let diags = ws.diagnostics("p");

    let key_diag = diags
        .iter()
        .find(|d| format!("{:?}", d.code) == "InvalidWritePath")
        .expect("expected an InvalidWritePath diagnostic on the statement key");
    assert_eq!(key_diag.location.expression_id.as_deref(), Some("s1"));
    assert!(
        matches!(&key_diag.location.target, Some(CursorTarget::ExpressionKey)),
        "key diagnostic must target the expression key, got {:?}",
        key_diag.location.target
    );

    let value_diag = diags
        .iter()
        .find(|d| {
            d.location.expression_id.as_deref() == Some("s2")
                && format!("{:?}", d.code) == "UndefinedVariable"
        })
        .expect("expected an UndefinedVariable diagnostic on the statement value");
    assert!(
        value_diag.location.target.is_none(),
        "value diagnostic must carry no target (defaults to the value expression), got {:?}",
        value_diag.location.target
    );
}

#[test]
fn undefined_member_emits_single_diagnostic_not_duplicate() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(json!({
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "tree", "type": "expression", "props": { "data": { "key": "customer.result", "value": "customer.agee + 1" } } }
            ]
        }))
        .unwrap(),
    );

    let undefined: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| format!("{:?}", d.code) == "UndefinedVariable")
        .collect();
    assert_eq!(
        undefined.len(),
        1,
        "a single undefined member must not be reported twice, got {undefined:#?}"
    );
}

#[test]
fn bare_root_read_is_not_an_unknown_property() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(json!({
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "e1", "type": "expression", "props": { "data": { "key": "customer.rootKeys", "value": "len(keys($root))" } } }
            ]
        }))
        .unwrap(),
    );

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "bare $root must not be flagged: {errors:#?}"
    );
}

#[test]
fn undefined_bare_identifier_still_reported_by_read_check() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(json!({
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "tree", "type": "expression", "props": { "data": { "key": "customer.result", "value": "nonexistentVar + 1" } } }
            ]
        }))
        .unwrap(),
    );

    let undefined: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| format!("{:?}", d.code) == "UndefinedVariable")
        .collect();
    assert_eq!(
        undefined.len(),
        1,
        "a bare undefined identifier the type-checker can't see must still be flagged, got {undefined:#?}"
    );
    assert!(undefined[0].message.contains("nonexistentVar"));
}

#[test]
fn input_override_on_expression_key_carries_key_target() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(json!({
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "s1", "type": "expression", "props": { "data": { "key": "customer.age", "value": "5" } } }
            ]
        }))
        .unwrap(),
    );
    let d = ws
        .diagnostics("p")
        .into_iter()
        .find(|d| format!("{:?}", d.code) == "InputOverride")
        .expect("expected InputOverride");
    assert!(
        matches!(&d.location.target, Some(CursorTarget::ExpressionKey)),
        "InputOverride must target the expression key, got {:?}",
        d.location.target
    );
}

#[test]
fn incompatible_key_type_carries_key_target_and_span() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "p",
        serde_json::from_value(json!({
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "customer",
                    "properties": [
                        { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "tree", "type": "match", "props": { "data": { "key": "customer.tag", "arms": [
                    { "id": "b1", "condition": "customer.age > 18", "value": "\"adult\"" },
                    { "id": "b2", "condition": "", "value": "1" }
                ]}}}
            ]
        }))
        .unwrap(),
    );
    let d = ws
        .diagnostics("p")
        .into_iter()
        .find(|d| format!("{:?}", d.code) == "TypeMismatch")
        .expect("expected TypeMismatch on incompatible key types");
    assert!(
        matches!(&d.location.target, Some(CursorTarget::MatchTarget)),
        "type-mismatch must target the match key, got {:?}",
        d.location.target
    );
    assert_eq!(
        d.location.span,
        Some((0, "customer.tag".chars().count() as u32)),
        "type-mismatch must span the key field"
    );
}

#[test]
fn unrelated_policies_do_not_cross_flag_writes() {
    let mk = || {
        json!({
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": json!({
                    "name": "customer",
                    "properties": [ { "id": "p1", "name": "income", "type": "number", "array": false, "optional": false } ]
                })}},
                { "id": "a", "type": "assertion", "props": { "data": json!({
                    "output": "customer.score",
                    "conditions": [ { "id": "c1", "expression": "customer.income > 0", "operator": "and", "depth": 0 } ]
                })}}
            ]
        })
    };
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("alpha", serde_json::from_value(mk()).unwrap());
    ws.set_policy("beta", serde_json::from_value(mk()).unwrap());

    for path in ["alpha", "beta"] {
        let errors: Vec<_> = ws
            .diagnostics(path)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "[{path}] unrelated policy writing the same path must not be flagged; got {errors:#?}",
        );
    }
}

#[test]
fn disjoint_nested_writes_across_blocks_merge_at_runtime() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "globals", "scope": "global",
                "properties": [ { "id": "g1", "name": "principal", "type": "number", "array": false, "optional": false } ]
            })}},
            { "id": "current", "type": "expression", "props": { "data": json!({
                "key": "portfolio.byBucket.CURRENT.balance", "value": "principal * 0.7"
            })}},
            { "id": "late", "type": "expression", "props": { "data": json!({
                "key": "portfolio.byBucket.LATE.balance", "value": "principal * 0.3"
            })}}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "disjoint assembly must be clean; got {errors:#?}"
    );

    let result = ws
        .evaluate(&EvaluateRequest {
            policy_path: Arc::from("p"),
            input: Variable::from(json!({ "principal": 1000 })),
            goals: Vec::new(),
            trace: false,
        })
        .expect("evaluate should succeed");
    let out: serde_json::Value = result.output.into();
    let bucket = out
        .get("portfolio")
        .and_then(|p| p.get("byBucket"))
        .and_then(|b| b.as_object())
        .expect("portfolio.byBucket assembled");
    assert_eq!(
        bucket
            .get("CURRENT")
            .and_then(|c| c.get("balance"))
            .and_then(|v| v.as_f64()),
        Some(700.0),
        "both blocks' disjoint nested writes must survive the merge; got {out}",
    );
    assert_eq!(
        bucket
            .get("LATE")
            .and_then(|c| c.get("balance"))
            .and_then(|v| v.as_f64()),
        Some(300.0),
        "both blocks' disjoint nested writes must survive the merge; got {out}",
    );
}

#[test]
fn reading_assembled_object_is_self_reference_not_cycle() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "globals", "scope": "global",
                "properties": [ { "id": "g1", "name": "principal", "type": "number", "array": false, "optional": false } ]
            })}},
            { "id": "s1", "type": "expression", "props": { "data": json!({ "key": "totals.base", "value": "principal" })}},
            { "id": "s2", "type": "expression", "props": { "data": json!({ "key": "totals.count", "value": "len(keys(totals))" })}}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let codes: Vec<String> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{:?}", d.code))
        .collect();
    assert!(
        codes.iter().any(|c| c == "SelfReferencingWrite"),
        "reading the object being assembled should surface as SelfReferencingWrite; got {codes:?}",
    );
    assert!(
        !codes.iter().any(|c| c == "CyclicDependency"),
        "it must not surface as a cryptic CyclicDependency; got {codes:?}",
    );
}

#[test]
fn unrelated_policies_do_not_leak_computed_global_types() {
    let writer = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "scope": "global", "name": "g",
                "properties": [ { "id": "p1", "name": "score", "type": "number", "array": false, "optional": false } ]
            }) }},
            { "id": "t", "type": "decisionTable", "props": { "data": json!({
                "hitPolicy": "first",
                "inputs": [ { "id": "i1", "name": "", "field": "score" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "tier" } ],
                "rules": [
                    { "_id": "r1", "i1": ">= 100", "o1": "\"gold\"" },
                    { "_id": "r2", "i1": ">= 50",  "o1": "\"silver\"" },
                    { "_id": "r3", "i1": "",       "o1": "\"bronze\"" }
                ]
            }) }}
        ]
    });
    let reader = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "scope": "global", "name": "g",
                "properties": [ { "id": "p1", "name": "tier", "type": "string", "array": false, "optional": false } ]
            }) }},
            { "id": "t", "type": "decisionTable", "props": { "data": json!({
                "hitPolicy": "first",
                "inputs": [ { "id": "i1", "name": "", "field": "tier" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "discount" } ],
                "rules": [
                    { "_id": "r1", "i1": "\"platinum\"", "o1": "0.2" },
                    { "_id": "r2", "i1": "\"gold\"",     "o1": "0.1" }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("tier-writer", serde_json::from_value(writer).unwrap());
    ws.set_policy("tier-reader", serde_json::from_value(reader).unwrap());

    let diags = ws.diagnostics("tier-reader");
    assert!(
        diags.is_empty(),
        "tier-reader declares its own global tier:string and never imports tier-writer; \
         tier-writer's computed gold|silver|bronze enum must not narrow it. Got: {:?}",
        diags
            .iter()
            .map(|d| (&d.code, &d.message))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn assertion_nested_or_of_and_group() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "pa", "name": "a", "type": "boolean", "array": false, "optional": false },
                    { "id": "pb", "name": "b", "type": "boolean", "array": false, "optional": false },
                    { "id": "pc", "name": "c", "type": "boolean", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "customer.result",
                "conditions": [
                    { "id": "ca", "expression": "customer.a", "operator": "or",  "depth": 0 },
                    { "id": "cb", "expression": "customer.b", "operator": "and", "depth": 1 },
                    { "id": "cc", "expression": "customer.c", "operator": "and", "depth": 1 }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let eval = |a: bool, b: bool, c: bool| -> bool {
        let result = ws
            .evaluate(&EvaluateRequest {
                policy_path: Arc::from("p"),
                input: Variable::from(json!({ "customer": { "a": a, "b": b, "c": c } })),
                goals: Vec::new(),
                trace: false,
            })
            .expect("evaluate succeeded");
        result
            .output
            .dot("customer.result")
            .and_then(|v| v.as_bool())
            .expect("customer.result is a bool")
    };

    assert!(eval(true, false, false), "true OR (false AND false) = true");
    assert!(eval(false, true, true), "false OR (true AND true) = true");
    assert!(
        !eval(false, true, false),
        "false OR (true AND false) = false"
    );
    assert!(
        !eval(false, false, false),
        "false OR (false AND false) = false"
    );
    assert!(eval(true, true, true), "true OR (true AND true) = true");
}

fn expr(id: &str, key: &str, value: &str) -> serde_json::Value {
    json!({ "id": id, "type": "expression", "props": { "data": { "key": key, "value": value } } })
}

fn match_block(id: &str, key: &str, arms: serde_json::Value) -> serde_json::Value {
    json!({ "id": id, "type": "match", "props": { "data": { "key": key, "arms": arms } } })
}

fn arm(id: &str, condition: &str, value: &str) -> serde_json::Value {
    json!({ "id": id, "condition": condition, "value": value })
}

fn customer_dm() -> serde_json::Value {
    json!({
        "id": "dm", "type": "dataModel", "props": { "data": {
            "name": "customer",
            "properties": [
                { "id": "a", "name": "age", "type": "number" },
                { "id": "i", "name": "income", "type": "number" }
            ]
        }}
    })
}

fn errors(ws: &PolicyWorkspace, path: &str) -> Vec<String> {
    ws.diagnostics(path)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{:?}: {}", d.code, d.message))
        .collect()
}

fn set(ws: &mut PolicyWorkspace, path: &str, blocks: serde_json::Value) {
    ws.set_policy(
        path,
        serde_json::from_value(json!({ "blocks": blocks })).unwrap(),
    );
}

fn eval(ws: &PolicyWorkspace, path: &str, input: serde_json::Value) -> serde_json::Value {
    let req = EvaluateRequest {
        policy_path: Arc::from(path),
        input: Variable::from(input),
        goals: Vec::new(),
        trace: false,
    };
    ws.evaluate(&req).expect("evaluate ok").output.into()
}

#[test]
fn expression_block_writes_and_types() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr("e1", "customer.bonus", "customer.income * 0.1")
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));

    let outputs = ws.outputs(&ScopeRequest::for_policy("p"));
    let bonus = outputs
        .iter()
        .find(|o| o.path.as_ref() == "customer.bonus")
        .expect("bonus output");
    assert_eq!(format!("{}", bonus.resolved_type), "number");

    let out = eval(
        &ws,
        "p",
        json!({ "customer": { "age": 40, "income": 1000 } }),
    );
    assert_eq!(out["customer"]["bonus"].as_f64(), Some(100.0));
}

#[test]
fn separate_expression_blocks_order_independently_no_false_cycle() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr("b", "customer.b", "customer.a + 1"),
            expr("c", "customer.c", "customer.income + 5"),
            expr("a", "customer.a", "customer.c + 1"),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));

    let out = eval(&ws, "p", json!({ "customer": { "age": 1, "income": 10 } }));
    assert_eq!(out["customer"]["c"].as_f64(), Some(15.0));
    assert_eq!(out["customer"]["a"].as_f64(), Some(16.0));
    assert_eq!(out["customer"]["b"].as_f64(), Some(17.0));
}

#[test]
fn genuine_cycle_is_reported() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr("a", "customer.a", "customer.b + 1"),
            expr("b", "customer.b", "customer.a + 1"),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("Cyclic")),
        "expected cycle, got {errs:?}"
    );
}

#[test]
fn duplicate_writer_across_expression_blocks() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr("e1", "customer.tier", "\"a\""),
            expr("e2", "customer.tier", "\"b\""),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("DuplicateWriter")),
        "{errs:?}"
    );
}

#[test]
fn match_with_default_is_total_and_merges_types() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm("a1", "customer.age >= 65", "\"senior\""),
                    arm("a2", "customer.age >= 18", "\"adult\""),
                    arm("a3", "", "\"minor\""),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));

    let outputs = ws.outputs(&ScopeRequest::for_policy("p"));
    let tier = outputs
        .iter()
        .find(|o| o.path.as_ref() == "customer.tier")
        .expect("tier output");
    let ty = format!("{}", tier.resolved_type);
    assert!(
        ty.contains("senior") && ty.contains("adult") && ty.contains("minor"),
        "{ty}"
    );
    assert!(!ty.contains('?'), "total match must not be nullable: {ty}");

    assert_eq!(
        eval(&ws, "p", json!({ "customer": { "age": 70 } }))["customer"]["tier"],
        json!("senior")
    );
    assert_eq!(
        eval(&ws, "p", json!({ "customer": { "age": 30 } }))["customer"]["tier"],
        json!("adult")
    );
    assert_eq!(
        eval(&ws, "p", json!({ "customer": { "age": 5 } }))["customer"]["tier"],
        json!("minor")
    );
}

#[test]
fn match_without_default_errors_and_returns_null_at_runtime() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([arm("a1", "customer.age >= 65", "\"senior\""),])
            ),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("MissingDefaultBranch")),
        "non-exhaustive match must require a default arm: {errs:?}"
    );

    let out = eval(&ws, "p", json!({ "customer": { "age": 5 } }));
    assert_eq!(out["customer"]["tier"], json!(null));
}

#[test]
fn match_non_boolean_condition_errors() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([arm("a1", "customer.age", "\"x\""), arm("a2", "", "\"y\""),])
            ),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter()
            .any(|e| e.contains("TypeMismatch") && e.contains("boolean")),
        "{errs:?}"
    );
}

#[test]
fn match_feeds_downstream_expression() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr("e", "customer.discounted", "customer.rate < 0.1"),
            match_block(
                "m",
                "customer.rate",
                json!([
                    arm("a1", "customer.age >= 65", "0.05"),
                    arm("a2", "", "0.2"),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    let out = eval(&ws, "p", json!({ "customer": { "age": 70 } }));
    assert_eq!(out["customer"]["rate"].as_f64(), Some(0.05));
    assert_eq!(out["customer"]["discounted"], json!(true));
}

#[test]
fn cross_block_deep_read_of_whole_object_write() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr(
                "e1",
                "customer.profile",
                "{ age: customer.age, vip: customer.income > 1000 }"
            ),
            expr("e2", "customer.adult", "customer.profile.age >= 18"),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    let out = eval(
        &ws,
        "p",
        json!({ "customer": { "age": 25, "income": 500 } }),
    );
    assert_eq!(out["customer"]["adult"], json!(true));
}

#[test]
fn cross_block_condition_sharing_via_intermediate() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr(
                "d",
                "customer.isPremium",
                "customer.age >= 65 and customer.income > 1000"
            ),
            match_block(
                "m1",
                "customer.tier",
                json!([
                    arm("a1", "customer.isPremium", "\"gold\""),
                    arm("a2", "", "\"std\"")
                ])
            ),
            match_block(
                "m2",
                "customer.discount",
                json!([arm("b1", "customer.isPremium", "0.2"), arm("b2", "", "0.0")])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    let out = eval(
        &ws,
        "p",
        json!({ "customer": { "age": 70, "income": 5000 } }),
    );
    assert_eq!(out["customer"]["tier"], json!("gold"));
    assert_eq!(out["customer"]["discount"].as_f64(), Some(0.2));
}

fn disc_dm() -> serde_json::Value {
    json!({
        "id": "dm", "type": "dataModel", "props": { "data": {
            "name": "customer",
            "properties": [
                { "id": "g", "name": "segment", "type": "string", "enum": ["retail", "corporate", "other"] },
                { "id": "ac", "name": "active", "type": "boolean" },
                { "id": "a", "name": "age", "type": "number" },
                { "id": "r", "name": "region", "type": "string" }
            ]
        }}
    })
}

fn tier_type(ws: &PolicyWorkspace) -> String {
    let outputs = ws.outputs(&ScopeRequest::for_policy("p"));
    let tier = outputs
        .iter()
        .find(|o| o.path.as_ref() == "customer.tier")
        .expect("tier output");
    format!("{}", tier.resolved_type)
}

#[test]
fn match_enum_full_cover_is_total_without_default() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm("a1", "customer.segment == \"retail\"", "\"gold\""),
                    arm("a2", "customer.segment == \"corporate\"", "\"silver\""),
                    arm("a3", "customer.segment == \"other\"", "\"bronze\""),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    let ty = tier_type(&ws);
    assert!(!ty.contains('?'), "full enum cover must be total: {ty}");
    assert_eq!(
        eval(&ws, "p", json!({ "customer": { "segment": "corporate" } }))["customer"]["tier"],
        json!("silver")
    );
}

#[test]
fn match_enum_partial_cover_errors_missing_default() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm("a1", "customer.segment == \"retail\"", "\"gold\""),
                    arm("a2", "customer.segment == \"corporate\"", "\"silver\""),
                ])
            ),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("MissingDefaultBranch")),
        "partial enum cover must require a default arm: {errs:?}"
    );
}

#[test]
fn match_in_set_covers_enum() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([arm(
                    "a1",
                    "customer.segment in [\"retail\", \"corporate\", \"other\"]",
                    "\"any\""
                ),])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    assert!(
        !tier_type(&ws).contains('?'),
        "in-set covering the enum must be total"
    );
}

#[test]
fn match_bool_both_arms_is_total() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm("a1", "customer.active == true", "\"on\""),
                    arm("a2", "customer.active == false", "\"off\""),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    assert!(
        !tier_type(&ws).contains('?'),
        "bool true+false must be total"
    );
}

#[test]
fn match_bool_one_arm_errors_missing_default() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([arm("a1", "customer.active == true", "\"on\""),])
            ),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("MissingDefaultBranch")),
        "single bool arm must require a default arm: {errs:?}"
    );
}

#[test]
fn match_number_tiling_no_gap_is_total() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm("a1", "customer.age < 18", "\"minor\""),
                    arm("a2", "customer.age >= 18", "\"adult\""),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    assert!(
        !tier_type(&ws).contains('?'),
        "gap-free number tiling must be total"
    );
}

#[test]
fn match_number_tiling_with_gap_errors_missing_default() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm("a1", "customer.age < 18", "\"minor\""),
                    arm("a2", "customer.age > 18", "\"adult\""),
                ])
            ),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("MissingDefaultBranch")),
        "number tiling with a gap must require a default arm: {errs:?}"
    );
}

#[test]
fn match_guard_does_not_count_errors_missing_default() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm(
                        "a1",
                        "customer.segment == \"retail\" and customer.region == \"EU\"",
                        "\"gold\""
                    ),
                    arm("a2", "customer.segment == \"corporate\"", "\"silver\""),
                    arm("a3", "customer.segment == \"other\"", "\"bronze\""),
                ])
            ),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("MissingDefaultBranch")),
        "a guarded arm is not a pure discriminant test, so a default arm is required: {errs:?}"
    );
}

#[test]
fn match_different_property_errors_missing_default() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm("a1", "customer.segment == \"retail\"", "\"gold\""),
                    arm("a2", "customer.active == false", "\"silver\""),
                ])
            ),
        ]),
    );
    let errs = errors(&ws, "p");
    assert!(
        errs.iter().any(|e| e.contains("MissingDefaultBranch")),
        "arms on different properties have no single discriminant, so a default arm is required: {errs:?}"
    );
}

#[test]
fn lazy_prunes_dead_arm_intermediate() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            expr("retail", "customer.retailValue", "customer.age * 2"),
            expr("corporate", "customer.corporateValue", "customer.age * 3"),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm(
                        "a1",
                        "customer.segment == \"retail\"",
                        "customer.retailValue"
                    ),
                    arm(
                        "a2",
                        "customer.segment == \"corporate\"",
                        "customer.corporateValue"
                    ),
                    arm("a3", "", "0"),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));

    let out = eval(
        &ws,
        "p",
        json!({ "customer": { "segment": "corporate", "age": 10 } }),
    );
    assert_eq!(out["customer"]["tier"].as_f64(), Some(30.0));
    assert_eq!(out["customer"]["corporateValue"].as_f64(), Some(30.0));
    assert!(
        out["customer"].get("retailValue").is_none(),
        "dead-arm intermediate must be pruned; got {out}"
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "segment": "corporate", "age": 10 } })),
        goals: vec![Arc::from("customer.retailValue")],
        trace: false,
    };
    let goal_out: serde_json::Value = ws.evaluate(&req).expect("goal eval ok").output.into();
    assert_eq!(
        goal_out["customer"]["retailValue"].as_f64(),
        Some(20.0),
        "a property promoted to a goal is forced even when only a dead arm reads it"
    );
}

#[test]
fn lazy_equals_eager_for_branch_free_policy() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            disc_dm(),
            expr("a", "customer.doubled", "customer.age * 2"),
            expr("b", "customer.tier", "customer.doubled + 1"),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    let out = eval(
        &ws,
        "p",
        json!({ "customer": { "segment": "retail", "age": 5 } }),
    );
    assert_eq!(out["customer"]["doubled"].as_f64(), Some(10.0));
    assert_eq!(out["customer"]["tier"].as_f64(), Some(11.0));
}

#[test]
fn lazy_iterated_match_picks_per_instance_arm() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "region", "type": "string", "enum": ["us", "eu"], "array": false, "optional": false },
                    { "id": "p3", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "us", "type": "expression", "props": { "data": json!({ "key": "company.usBonus", "value": "company.revenue * 2" }) }},
            { "id": "eu", "type": "expression", "props": { "data": json!({ "key": "company.euBonus", "value": "company.revenue * 3" }) }},
            { "id": "m", "type": "match", "props": { "data": json!({
                "key": "company.bonus",
                "arms": [
                    { "id": "a1", "condition": "company.region == \"us\"", "value": "company.usBonus" },
                    { "id": "a2", "condition": "company.region == \"eu\"", "value": "company.euBonus" },
                    { "id": "a3", "condition": "", "value": "0" }
                ]
            }) }}
        ]
    });

    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let errs: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "expected no errors, got {errs:#?}");

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({
            "customer": { "companies": [
                { "region": "us", "revenue": 100 },
                { "region": "eu", "revenue": 100 }
            ] }
        })),
        goals: Vec::new(),
        trace: false,
    };
    let serialized: serde_json::Value = ws.evaluate(&req).expect("evaluate ok").output.into();
    let companies = serialized
        .pointer("/customer/companies")
        .and_then(|v| v.as_array())
        .expect("companies present");
    assert_eq!(
        companies[0]["bonus"].as_f64(),
        Some(200.0),
        "us company resolves the us arm"
    );
    assert_eq!(
        companies[1]["bonus"].as_f64(),
        Some(300.0),
        "eu company resolves the eu arm"
    );
}

fn group_paths(group: &serde_json::Value, key: &str) -> Vec<String> {
    group[key]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|p| p["path"].as_str().map(String::from))
        .collect()
}

#[test]
fn conditional_schema_clean_enum_discriminant_union() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            json!({ "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "g", "name": "segment", "type": "string", "enum": ["retail", "corporate"] },
                    { "id": "sd", "name": "retailData", "type": "number" },
                    { "id": "ad", "name": "corporateData", "type": "number" }
                ]
            }}}),
            expr("scalc", "customer.retailCalc", "customer.retailData * 2"),
            expr(
                "acalc",
                "customer.corporateCalc",
                "customer.corporateData * 2"
            ),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm(
                        "a1",
                        "customer.segment == \"retail\"",
                        "customer.retailCalc"
                    ),
                    arm(
                        "a2",
                        "customer.segment == \"corporate\"",
                        "customer.corporateCalc"
                    ),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));

    let schema =
        serde_json::to_value(ws.conditional_schema(&ScopeRequest::for_policy("p"))).unwrap();
    assert_eq!(schema["kind"], "union");
    assert_eq!(schema["union"]["property"], "customer.segment");

    let common_inputs = group_paths(&schema["common"], "inputs");
    let common_outputs = group_paths(&schema["common"], "outputs");
    assert!(
        common_inputs.contains(&"customer.segment".to_string()),
        "{common_inputs:?}"
    );
    assert!(
        common_outputs.contains(&"customer.tier".to_string()),
        "{common_outputs:?}"
    );

    let variants = schema["union"]["variants"].as_array().unwrap();
    let retail = variants
        .iter()
        .find(|v| v["value"] == "retail")
        .expect("retail variant");
    let sag_inputs = group_paths(&retail["group"], "inputs");
    let sag_outputs = group_paths(&retail["group"], "outputs");
    assert!(
        sag_inputs.contains(&"customer.retailData".to_string()),
        "{sag_inputs:?}"
    );
    assert!(
        !sag_inputs.contains(&"customer.corporateData".to_string()),
        "retail variant must not carry the corporate input: {sag_inputs:?}"
    );
    assert!(
        sag_outputs.contains(&"customer.retailCalc".to_string()),
        "{sag_outputs:?}"
    );

    let corporate = variants
        .iter()
        .find(|v| v["value"] == "corporate")
        .expect("corporate variant");
    assert!(
        group_paths(&corporate["group"], "inputs").contains(&"customer.corporateData".to_string())
    );

    let all_inputs: std::collections::HashSet<String> = ws
        .inputs(&ScopeRequest::for_policy("p"))
        .iter()
        .map(|i| i.path.to_string())
        .collect();
    let mut partitioned: std::collections::HashSet<String> = common_inputs.into_iter().collect();
    for v in variants {
        partitioned.extend(group_paths(&v["group"], "inputs"));
    }
    assert_eq!(
        partitioned, all_inputs,
        "common + variants must cover the flat input superset"
    );
}

#[test]
fn conditional_schema_compound_condition_is_flat_with_guard() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            json!({ "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "g", "name": "segment", "type": "string", "enum": ["retail", "corporate"] },
                    { "id": "r", "name": "region", "type": "string" },
                    { "id": "sd", "name": "retailData", "type": "number" }
                ]
            }}}),
            expr("scalc", "customer.retailCalc", "customer.retailData * 2"),
            match_block(
                "m",
                "customer.tier",
                json!([
                    arm(
                        "a1",
                        "customer.segment == \"retail\" and customer.region == \"EU\"",
                        "customer.retailCalc"
                    ),
                    arm("a2", "", "0"),
                ])
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    let schema =
        serde_json::to_value(ws.conditional_schema(&ScopeRequest::for_policy("p"))).unwrap();
    assert_eq!(
        schema["kind"], "flat",
        "compound guard has no single clean discriminant"
    );

    let cond_inputs = schema["conditional"]["inputs"].as_array().unwrap();
    let retail = cond_inputs
        .iter()
        .find(|p| p["path"] == "customer.retailData")
        .expect("retailData is conditional");
    assert!(
        retail["requiredWhen"]
            .as_str()
            .unwrap_or_default()
            .contains("region"),
        "guard should carry the compound condition: {retail}"
    );
}

#[test]
fn conditional_schema_branch_free_is_flat_with_empty_conditional() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            customer_dm(),
            expr("a", "customer.doubled", "customer.age * 2"),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));
    let schema =
        serde_json::to_value(ws.conditional_schema(&ScopeRequest::for_policy("p"))).unwrap();
    assert_eq!(schema["kind"], "flat");
    assert!(schema["conditional"]["inputs"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(schema["conditional"]["outputs"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn global_aggregation_of_fanout_field() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "fee", "type": "expression", "props": { "data": json!({ "key": "company.fee", "value": "company.revenue * 2" }) }},
            { "id": "total", "type": "expression", "props": { "data": json!({ "key": "totalFee", "value": "sum(map(customer.companies as c, c.fee))" }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let errs: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "expected no errors, got {errs:#?}");

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(
            json!({ "customer": { "companies": [ { "revenue": 10 }, { "revenue": 20 } ] } }),
        ),
        goals: Vec::new(),
        trace: false,
    };
    let out: serde_json::Value = ws.evaluate(&req).expect("evaluate ok").output.into();
    assert_eq!(
        out["totalFee"].as_f64(),
        Some(60.0),
        "global aggregation must demand the per-entity fan-out write; got {out}"
    );
}

#[test]
fn trace_on_error_carries_partial_trace() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "p",
        json!([
            json!({ "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "a", "name": "age", "type": "number" },
                    { "id": "n", "name": "name", "type": "string" }
                ]
            }}}),
            expr("ok", "okValue", "42"),
            expr(
                "bad",
                "customer.bad",
                "okValue > 0 ? sum([customer.age, customer.name]) : 0"
            ),
        ]),
    );
    assert!(errors(&ws, "p").is_empty(), "{:?}", errors(&ws, "p"));

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "age": 10, "name": "Alice" } })),
        goals: Vec::new(),
        trace: true,
    };
    match ws
        .evaluate(&req)
        .expect_err("summing a non-numeric array must fail at runtime")
    {
        EvaluationError::ExpressionFailed {
            block_id,
            partial_trace,
            ..
        } => {
            assert_eq!(block_id.as_ref(), "bad", "error names the failing block");
            let trace = partial_trace.expect("partial trace attached on error");
            assert!(
                trace.executions.iter().any(|e| e.block_id.as_ref() == "ok"),
                "partial trace must include blocks that ran before the failure; got {:?}",
                trace
                    .executions
                    .iter()
                    .map(|e| e.block_id.clone())
                    .collect::<Vec<_>>()
            );
        }
        other => panic!("expected ExpressionFailed, got {other:?}"),
    }
}

#[test]
fn goal_validation_resolves_fanout_entity_input() {
    let doc = json!({
        "blocks": [
            { "id": "dm-app", "type": "dataModel", "props": { "data": json!({
                "name": "application",
                "properties": [
                    { "id": "d", "name": "drivers", "type": "relationship", "target": "driver", "array": true, "optional": false }
                ]
            }) }},
            { "id": "dm-driver", "type": "dataModel", "props": { "data": json!({
                "name": "driver",
                "properties": [
                    { "id": "dob", "name": "dob", "type": "number", "array": false, "optional": false }
                ]
            }) }},
            { "id": "age", "type": "expression", "props": { "data": json!({ "key": "driver.age", "value": "2025 - driver.dob" }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    let errs: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "expected no errors, got {errs:#?}");

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "application": { "drivers": [ { "dob": 1990 } ] } })),
        goals: vec![Arc::from("driver.age")],
        trace: false,
    };
    let result = ws.evaluate(&req);
    assert!(
        result.is_ok(),
        "fan-out entity field input must satisfy the goal's required input; got {result:?}"
    );
    let out: serde_json::Value = result.unwrap().output.into();
    assert_eq!(
        out.pointer("/application/drivers/0/age")
            .and_then(|v| v.as_f64()),
        Some(35.0)
    );
}

#[test]
fn cross_component_duplicate_writer_detected() {
    let mut ws = PolicyWorkspace::new();
    set(
        &mut ws,
        "a",
        json!([customer_dm(), expr("e", "customer.tier", "\"gold\"")]),
    );
    set(
        &mut ws,
        "b",
        json!([customer_dm(), expr("e", "customer.tier", "\"silver\"")]),
    );

    let conflicts = ws.cross_component_write_conflicts();
    let tier = conflicts
        .iter()
        .find(|c| c.path.as_ref() == "customer.tier")
        .expect("cross-component conflict on customer.tier");
    assert!(
        tier.policies.iter().any(|p| p.as_ref() == "a"),
        "{:?}",
        tier.policies
    );
    assert!(
        tier.policies.iter().any(|p| p.as_ref() == "b"),
        "{:?}",
        tier.policies
    );
}

#[test]
fn cross_component_no_conflict_when_imported() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "base",
        serde_json::from_value(
            json!({ "blocks": [ customer_dm(), expr("e", "customer.x", "1") ] }),
        )
        .unwrap(),
    );
    ws.set_policy("mid", serde_json::from_value(json!({ "imports": ["base"], "blocks": [ expr("e2", "customer.y", "customer.x + 1") ] })).unwrap());
    assert!(
        ws.cross_component_write_conflicts().is_empty(),
        "imported policies share a component, no conflict"
    );
}

#[test]
fn component_members_lists_merge_set() {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "base",
        serde_json::from_value(
            json!({ "blocks": [ customer_dm(), expr("e", "customer.x", "1") ] }),
        )
        .unwrap(),
    );
    ws.set_policy("mid", serde_json::from_value(json!({ "imports": ["base"], "blocks": [ expr("e2", "customer.y", "customer.x + 1") ] })).unwrap());
    ws.set_policy(
        "solo",
        serde_json::from_value(json!({ "blocks": [ customer_dm() ] })).unwrap(),
    );

    let members: Vec<String> = ws
        .component_members("mid")
        .iter()
        .map(|m| m.to_string())
        .collect();
    assert_eq!(
        members,
        vec!["base".to_string(), "mid".to_string()],
        "mid merges with base"
    );
    assert_eq!(
        ws.component_members("solo")
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>(),
        vec!["solo".to_string()],
        "unrelated policy is its own component"
    );
}

#[test]
fn reference_pool_field_read_terminates_and_evaluates() {
    let doc = json!({
        "blocks": [
            { "id": "dm-order", "type": "dataModel", "props": { "data": json!({
                "name": "order",
                "properties": [
                    { "id": "p1", "name": "productId", "type": "reference", "target": "product" },
                    { "id": "p2", "name": "qty", "type": "number" }
                ]
            }) }},
            { "id": "dm-product", "type": "dataModel", "props": { "data": json!({
                "name": "product",
                "properties": [
                    { "id": "p3", "name": "id", "type": "string" },
                    { "id": "p4", "name": "price", "type": "number" }
                ]
            }) }},
            { "id": "e-total", "type": "expression", "props": { "data": json!({
                "key": "order.total",
                "value": "(product[0].price ?? 0) * order.qty"
            }) }},
            { "id": "e-premium", "type": "expression", "props": { "data": json!({
                "key": "product.premium",
                "value": "product.price > 100"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let _ = ws.all_diagnostics();

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({
            "order": { "productId": "p1", "qty": 2 },
            "product": [ { "id": "p1", "price": 10 }, { "id": "p2", "price": 200 } ]
        })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/order/total"), Some(&json!(20)));
    assert_eq!(output.pointer("/product/0/premium"), Some(&json!(false)));
    assert_eq!(output.pointer("/product/1/premium"), Some(&json!(true)));
}

#[test]
fn nested_relationship_chain_writes_are_flagged() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true }
                ]
            }) }},
            { "id": "dm-company", "type": "dataModel", "props": { "data": json!({
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "branches", "type": "relationship", "target": "branch", "array": true }
                ]
            }) }},
            { "id": "dm-branch", "type": "dataModel", "props": { "data": json!({
                "name": "branch",
                "properties": [
                    { "id": "p3", "name": "headcount", "type": "number" }
                ]
            }) }},
            { "id": "e-score", "type": "expression", "props": { "data": json!({
                "key": "branch.score",
                "value": "branch.headcount * 2"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let nested: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| {
            d.code == zen_engine::policy::DiagnosticCode::UnsupportedNestedIteration
                && d.severity == Severity::Error
        })
        .collect();
    assert_eq!(
        nested.len(),
        1,
        "writes behind a two-level relationship chain must be rejected, got {nested:#?}",
    );
}

#[test]
fn empty_match_arm_value_types_as_nullable() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "flag", "type": "boolean" }
                ]
            }) }},
            { "id": "m", "type": "match", "props": { "data": json!({
                "key": "customer.tier",
                "arms": [
                    { "id": "a1", "condition": "customer.flag", "value": "1" },
                    { "id": "a2", "condition": "", "value": "" }
                ]
            }) }},
            { "id": "e-next", "type": "expression", "props": { "data": json!({
                "key": "customer.next",
                "value": "customer.tier + 1"
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        !errors.is_empty(),
        "customer.tier can be null at runtime, so `customer.tier + 1` must be a type error",
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "flag": false } })),
        goals: vec![Arc::from("customer.tier")],
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/customer/tier"), Some(&json!(null)));
}

#[test]
fn invalid_write_paths_are_not_registered_as_writers() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number" }
                ]
            }) }},
            { "id": "e-bad", "type": "expression", "props": { "data": json!({
                "key": "total amount",
                "value": "customer.age * 2"
            }) }},
            { "id": "m-bad", "type": "match", "props": { "data": json!({
                "key": "a..b",
                "arms": [ { "id": "a1", "condition": "", "value": "1" } ]
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let invalid_paths = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| {
            d.code == zen_engine::policy::DiagnosticCode::InvalidWritePath
                && d.severity == Severity::Error
        })
        .count();
    assert_eq!(invalid_paths, 2);

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "age": 30 } })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert!(
        output.get("total amount").is_none(),
        "malformed expression key must not be written: {output:#?}",
    );
    assert!(
        output.get("a").is_none(),
        "malformed match key must not be written: {output:#?}",
    );
}

#[test]
fn rename_field_rewrites_nested_write_keys() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "base", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-total", "type": "expression", "props": { "data": {
                "key": "customer.scores.total",
                "value": "customer.base * 2"
            } } },
            { "id": "e-flat", "type": "expression", "props": { "data": {
                "key": "customer.scoresTotal",
                "value": "customer.base * 3"
            } } },
            { "id": "e-read", "type": "expression", "props": { "data": {
                "key": "customer.final",
                "value": "customer.scores.total + 1"
            } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let edits = ws.rename(
        &zen_engine::policy::RenameTarget::Field {
            entity: Arc::from("customer"),
            field: Arc::from("scores"),
        },
        "metrics",
    );
    let total_json = rewritten_block_json(&edits, "e-total")
        .unwrap_or_else(|| panic!("rename must rewrite the nested write key; got {edits:#?}"));
    assert!(
        total_json.contains("customer.metrics.total"),
        "write key `customer.scores.total` must become `customer.metrics.total`; got {total_json}",
    );
    let read_json = rewritten_block_json(&edits, "e-read")
        .unwrap_or_else(|| panic!("rename must rewrite the reader; got {edits:#?}"));
    assert!(
        read_json.contains("customer.metrics.total"),
        "read of `customer.scores.total` must follow the write key; got {read_json}",
    );
    assert!(
        !any_touches_block(&edits, "e-flat"),
        "write key `customer.scoresTotal` must not be treated as a prefix match; got {edits:#?}",
    );
}

#[test]
fn dependencies_resolves_nested_field_target_across_components() {
    let decoy = json!({
        "blocks": [
            { "id": "dm-other", "type": "dataModel", "props": { "data": {
                "name": "other",
                "properties": [
                    { "id": "p1", "name": "x", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-decoy", "type": "expression", "props": { "data": {
                "key": "other.y",
                "value": "other.x * 2"
            } } }
        ]
    });
    let writer = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p2", "name": "base", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-out", "type": "expression", "props": { "data": {
                "key": "customer.out",
                "value": "{ sub: customer.base * 2 }"
            } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("a-decoy", serde_json::from_value(decoy).unwrap());
    ws.set_policy("z-writer", serde_json::from_value(writer).unwrap());

    let node = ws.dependencies("customer.out.sub");
    let written_by = node
        .written_by
        .as_ref()
        .unwrap_or_else(|| panic!("customer.out.sub must resolve to its writer; got {node:#?}"));
    assert_eq!(written_by.policy_path.as_ref(), "z-writer");
    assert_eq!(written_by.block_id.as_ref(), "e-out");
    let dep_paths: Vec<String> = node.deps.iter().map(|d| d.property.to_string()).collect();
    assert!(
        dep_paths.iter().any(|p| p == "customer.base"),
        "nested target must decompose into the value expression's reads; got {dep_paths:?}",
    );
}

#[test]
fn null_typed_computed_field_reads_are_not_unknown_properties() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "base", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-null", "type": "expression", "props": { "data": {
                "key": "customer.flag",
                "value": "null"
            } } },
            { "id": "e-read", "type": "expression", "props": { "data": {
                "key": "customer.out",
                "value": "customer.flag ?? 'none'"
            } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let errors: Vec<_> = ws
        .diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "reading a null-typed computed field must not raise errors: {errors:#?}",
    );

    let req = EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(json!({ "customer": { "base": 1 } })),
        goals: Vec::new(),
        trace: false,
    };
    let result = ws.evaluate(&req).expect("policy must evaluate");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/customer/out"), Some(&json!("none")));
}

fn bool_assertion_eval(conditions: serde_json::Value) -> impl Fn(bool, bool, bool) -> bool {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": json!({
                "name": "customer",
                "properties": [
                    { "id": "pa", "name": "a", "type": "boolean", "array": false, "optional": false },
                    { "id": "pb", "name": "b", "type": "boolean", "array": false, "optional": false },
                    { "id": "pc", "name": "c", "type": "boolean", "array": false, "optional": false }
                ]
            }) }},
            { "id": "assert", "type": "assertion", "props": { "data": json!({
                "output": "customer.result",
                "conditions": conditions
            }) }}
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    move |a: bool, b: bool, c: bool| -> bool {
        let result = ws
            .evaluate(&EvaluateRequest {
                policy_path: Arc::from("p"),
                input: Variable::from(json!({ "customer": { "a": a, "b": b, "c": c } })),
                goals: Vec::new(),
                trace: false,
            })
            .expect("evaluate succeeded");
        result
            .output
            .dot("customer.result")
            .and_then(|v| v.as_bool())
            .expect("customer.result is a bool")
    }
}

#[test]
fn assertion_group_then_sibling_keeps_or_operator() {
    let eval = bool_assertion_eval(json!([
        { "id": "ca", "expression": "customer.a", "operator": "and", "depth": 1 },
        { "id": "cb", "expression": "customer.b", "operator": "or",  "depth": 1 },
        { "id": "cc", "expression": "customer.c", "operator": "and", "depth": 0 }
    ]));

    assert!(eval(false, false, true), "(false AND false) OR true = true");
    assert!(eval(true, true, false), "(true AND true) OR false = true");
    assert!(
        !eval(false, true, false),
        "(false AND true) OR false = false"
    );
    assert!(eval(true, true, true), "(true AND true) OR true = true");
}

#[test]
fn assertion_group_then_sibling_keeps_and_operator() {
    let eval = bool_assertion_eval(json!([
        { "id": "ca", "expression": "customer.a", "operator": "or",  "depth": 1 },
        { "id": "cb", "expression": "customer.b", "operator": "and", "depth": 1 },
        { "id": "cc", "expression": "customer.c", "operator": "and", "depth": 0 }
    ]));

    assert!(
        !eval(true, false, false),
        "(true OR false) AND false = false"
    );
    assert!(eval(true, false, true), "(true OR false) AND true = true");
    assert!(
        !eval(false, false, true),
        "(false OR false) AND true = false"
    );
}

#[test]
fn assertion_with_only_empty_conditions_still_writes_false() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "base", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "assert", "type": "assertion", "props": { "data": {
                "output": "customer.ok",
                "conditions": [
                    { "id": "c1", "expression": "", "operator": "and", "depth": 0 }
                ]
            } } },
            { "id": "e-read", "type": "expression", "props": { "data": {
                "key": "customer.okText",
                "value": "customer.ok ? 'yes' : 'no'"
            } } }
        ]
    });
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());

    let output = eval(&ws, "p", json!({ "customer": { "base": 1 } }));
    assert_eq!(
        output.pointer("/customer/ok"),
        Some(&json!(false)),
        "an assertion with no effective conditions must still write false: {output:#?}",
    );
    assert_eq!(output.pointer("/customer/okText"), Some(&json!("no")));
}

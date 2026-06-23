use serde_json::json;
use std::sync::Arc;
use zen_engine::policy::{EvaluateRequest, PolicyWorkspace};
use zen_expression::variable::Variable;

fn workspace_with(doc: serde_json::Value) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws
}

fn request(input: serde_json::Value, goals: Vec<&str>, trace: bool) -> EvaluateRequest {
    EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(input),
        goals: goals.into_iter().map(Arc::from).collect(),
        trace,
    }
}

fn executed_block_ids(ws: &PolicyWorkspace, req: &EvaluateRequest) -> Vec<String> {
    let result = ws.evaluate(req).expect("evaluate succeeded");
    result
        .trace
        .expect("trace populated")
        .executions
        .iter()
        .map(|e| e.block_id.to_string())
        .collect()
}

fn match_laziness_doc() -> serde_json::Value {
    json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "score", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-a", "type": "expression", "props": { "data": {
                "key": "customer.aVal",
                "value": "customer.score * 2"
            } } },
            { "id": "e-b", "type": "expression", "props": { "data": {
                "key": "customer.bVal",
                "value": "customer.score * 3"
            } } },
            { "id": "m", "type": "match", "props": { "data": {
                "key": "customer.out",
                "arms": [
                    { "id": "a1", "condition": "customer.score >= 10", "value": "customer.aVal" },
                    { "id": "a2", "condition": "", "value": "customer.bVal" }
                ]
            } } }
        ]
    })
}

#[test]
fn unmatched_match_arm_dependencies_do_not_execute() {
    let ws = workspace_with(match_laziness_doc());
    let req = request(json!({ "customer": { "score": 20 } }), vec![], true);
    let executed = executed_block_ids(&ws, &req);

    assert!(
        executed.contains(&"e-a".to_string()),
        "matched arm dependency must run: {executed:?}"
    );
    assert!(executed.contains(&"m".to_string()));
    assert!(
        !executed.contains(&"e-b".to_string()),
        "unmatched arm dependency must stay lazy: {executed:?}",
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/customer/out"), Some(&json!(40)));
    assert_eq!(output.pointer("/customer/bVal"), None);
}

#[test]
fn default_match_arm_dependencies_execute_when_selected() {
    let ws = workspace_with(match_laziness_doc());
    let req = request(json!({ "customer": { "score": 5 } }), vec![], true);
    let executed = executed_block_ids(&ws, &req);

    assert!(
        executed.contains(&"e-b".to_string()),
        "default arm dependency must run: {executed:?}"
    );
    assert!(
        !executed.contains(&"e-a".to_string()),
        "non-selected arm dependency must stay lazy: {executed:?}",
    );
}

#[test]
fn dependencies_execute_before_dependents() {
    let ws = workspace_with(match_laziness_doc());
    let req = request(json!({ "customer": { "score": 20 } }), vec![], true);
    let executed = executed_block_ids(&ws, &req);

    let pos = |id: &str| {
        executed
            .iter()
            .position(|e| e == id)
            .unwrap_or_else(|| panic!("{id} missing from {executed:?}"))
    };
    assert!(
        pos("e-a") < pos("m"),
        "dependency must commit before dependent: {executed:?}"
    );
}

#[test]
fn goals_prune_unrelated_blocks() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "score", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-x", "type": "expression", "props": { "data": {
                "key": "customer.x",
                "value": "customer.score * 2"
            } } },
            { "id": "e-y", "type": "expression", "props": { "data": {
                "key": "customer.y",
                "value": "customer.score * 3"
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(
        json!({ "customer": { "score": 10 } }),
        vec!["customer.x"],
        true,
    );
    let executed = executed_block_ids(&ws, &req);

    assert!(executed.contains(&"e-x".to_string()));
    assert!(
        !executed.contains(&"e-y".to_string()),
        "goal pruning must skip e-y: {executed:?}"
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/customer/x"), Some(&json!(20)));
    assert_eq!(output.pointer("/customer/y"), None);
}

#[test]
fn decision_table_unmatched_row_dependencies_do_not_execute() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-adult", "type": "expression", "props": { "data": {
                "key": "customer.adultMsg",
                "value": "\"adult\""
            } } },
            { "id": "e-child", "type": "expression", "props": { "data": {
                "key": "customer.childMsg",
                "value": "\"child\""
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "hitPolicy": "first",
                "inputs": [ { "id": "i1", "name": "", "field": "customer.age" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.msg" } ],
                "rules": [
                    { "i1": ">= 18", "o1": "customer.adultMsg" },
                    { "i1": "", "o1": "customer.childMsg" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "age": 30 } }), vec![], true);
    let executed = executed_block_ids(&ws, &req);

    assert!(
        executed.contains(&"e-adult".to_string()),
        "matched row dependency must run: {executed:?}"
    );
    assert!(
        !executed.contains(&"e-child".to_string()),
        "unmatched row dependency must stay lazy: {executed:?}",
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/customer/msg"), Some(&json!("adult")));
}

fn iterated_doc() -> serde_json::Value {
    json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "name", "type": "string", "array": false, "optional": false },
                    { "id": "p2", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            } } },
            { "id": "dm-company", "type": "dataModel", "props": { "data": {
                "name": "company",
                "properties": [
                    { "id": "p3", "name": "name", "type": "string", "array": false, "optional": false },
                    { "id": "p4", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-top", "type": "expression", "props": { "data": {
                "key": "company.isTop",
                "value": "max(map(company.customer.companies as c, c.revenue)) == company.revenue"
            } } }
        ]
    })
}

#[test]
fn iterated_owner_binding_does_not_leak_into_output() {
    let ws = workspace_with(iterated_doc());
    let input = json!({ "customer": { "name": "Alice", "companies": [
        { "name": "Acme", "revenue": 500 },
        { "name": "Mega", "revenue": 1000 }
    ] } });

    for trace in [false, true] {
        let req = request(input.clone(), vec![], trace);
        let result = ws.evaluate(&req).expect("evaluate succeeded");
        let output: serde_json::Value = result.output.into();
        let companies = output
            .pointer("/customer/companies")
            .and_then(|v| v.as_array())
            .expect("companies present");
        assert_eq!(companies.len(), 2);
        assert_eq!(companies[0].get("isTop"), Some(&json!(false)));
        assert_eq!(companies[1].get("isTop"), Some(&json!(true)));
        for company in companies {
            assert!(
                company.get("customer").is_none(),
                "synthetic owner binding leaked into output (trace={trace}): {company:#?}",
            );
        }
    }
}

#[test]
fn iterated_match_unselected_arm_dependencies_do_not_execute() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            } } },
            { "id": "dm-company", "type": "dataModel", "props": { "data": {
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-big", "type": "expression", "props": { "data": {
                "key": "customer.bigRate",
                "value": "2"
            } } },
            { "id": "e-small", "type": "expression", "props": { "data": {
                "key": "customer.smallRate",
                "value": "3"
            } } },
            { "id": "m", "type": "match", "props": { "data": {
                "key": "company.bonus",
                "arms": [
                    { "id": "hi", "condition": "company.revenue >= 100", "value": "company.revenue * customer.bigRate" },
                    { "id": "lo", "condition": "", "value": "company.revenue * customer.smallRate" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(
        json!({ "customer": { "companies": [ { "revenue": 200 }, { "revenue": 300 } ] } }),
        vec![],
        true,
    );
    let executed = executed_block_ids(&ws, &req);

    assert!(
        executed.contains(&"e-big".to_string()),
        "selected arm dependency must run: {executed:?}"
    );
    assert!(
        !executed.contains(&"e-small".to_string()),
        "arm unselected by every instance must stay lazy: {executed:?}",
    );
    assert_eq!(
        executed.iter().filter(|id| id.as_str() == "m").count(),
        2,
        "one execution per instance: {executed:?}",
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(
        output.pointer("/customer/companies/0/bonus"),
        Some(&json!(400))
    );
    assert_eq!(
        output.pointer("/customer/companies/1/bonus"),
        Some(&json!(600))
    );
}

#[test]
fn iterated_failure_does_not_leak_owner_binding_into_partial_trace() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            } } },
            { "id": "dm-company", "type": "dataModel", "props": { "data": {
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-fail", "type": "expression", "props": { "data": {
                "key": "company.score",
                "value": "company.customer.missing.deep + company.revenue"
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let input = json!({ "customer": { "companies": [ { "revenue": 200 } ] } });
    let req = request(input.clone(), vec![], true);

    let _ = ws.evaluate(&req);

    let probe = workspace_with(iterated_doc());
    let _ = probe;
    let input_after: serde_json::Value = req.input.into();
    let company = input_after
        .pointer("/customer/companies/0")
        .expect("instance present");
    assert!(
        company.get("customer").is_none(),
        "synthetic owner binding leaked into caller input after failure: {company:#?}",
    );
}

#[test]
fn enhance_trace_tolerates_runtime_error_in_post_selection_match_arm() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "flag", "type": "string", "array": false, "optional": false }
                ]
            } } },
            { "id": "m", "type": "match", "props": { "data": {
                "key": "customer.out",
                "arms": [
                    { "id": "a1", "condition": "customer.flag == ''", "value": "\"empty\"" },
                    { "id": "a2", "condition": "number(customer.flag) > 2", "value": "\"big\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "flag": "" } }), vec![], true);

    let plain = ws.evaluate(&req).expect("evaluate must succeed");
    let output: serde_json::Value = plain.output.into();
    assert_eq!(output.pointer("/customer/out"), Some(&json!("empty")));

    let enhanced = ws
        .enhance_trace(&req)
        .expect("enhance_trace must tolerate a post-selection arm error");
    let trace = enhanced.trace.expect("trace populated");
    let m = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "m")
        .expect("m missing from trace");
    let trace_json = serde_json::to_value(&m.trace).unwrap();
    assert_eq!(trace_json.pointer("/matchedArm"), Some(&json!("a1")));
    assert_eq!(
        trace_json.pointer("/arms"),
        Some(&json!([
            { "id": "a1", "result": true },
            { "id": "a2", "result": false }
        ]))
    );
}

#[test]
fn enhance_trace_propagates_runtime_error_before_match_selection() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "flag", "type": "string", "array": false, "optional": false }
                ]
            } } },
            { "id": "m", "type": "match", "props": { "data": {
                "key": "customer.out",
                "arms": [
                    { "id": "a1", "condition": "number(customer.flag) > 2", "value": "\"big\"" },
                    { "id": "a2", "condition": "", "value": "\"fallback\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "flag": "" } }), vec![], true);

    assert!(ws.evaluate(&req).is_err());
    assert!(
        ws.enhance_trace(&req).is_err(),
        "pre-selection arm error must still propagate in extras mode"
    );
}

#[test]
fn enhance_trace_tolerates_runtime_error_in_post_selection_table_row() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "flag", "type": "string", "array": false, "optional": false }
                ]
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "hitPolicy": "first",
                "inputs": [ { "id": "i1", "name": "", "field": "customer.flag" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.out" } ],
                "rules": [
                    { "i1": "== ''", "o1": "\"empty\"" },
                    { "i1": "number($) > 2", "o1": "\"big\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "flag": "" } }), vec![], true);

    let plain = ws.evaluate(&req).expect("evaluate must succeed");
    let output: serde_json::Value = plain.output.into();
    assert_eq!(output.pointer("/customer/out"), Some(&json!("empty")));

    let enhanced = ws
        .enhance_trace(&req)
        .expect("enhance_trace must tolerate a post-selection row error");
    let trace = enhanced.trace.expect("trace populated");
    let dt = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt")
        .expect("dt missing from trace");
    let trace_json = serde_json::to_value(&dt.trace).unwrap();
    assert_eq!(trace_json.pointer("/matchedRows"), Some(&json!([0])));
}

#[test]
fn enhance_trace_tolerates_error_after_failed_column_in_unmatched_row() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "flag", "type": "string", "array": false, "optional": false }
                ]
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "hitPolicy": "first",
                "inputs": [
                    { "id": "i1", "name": "", "field": "customer.flag" },
                    { "id": "i2", "name": "", "field": "customer.flag" }
                ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.out" } ],
                "rules": [
                    { "i1": "== 'x'", "i2": "number($) > 2", "o1": "\"first\"" },
                    { "i1": "== ''", "o1": "\"second\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "flag": "" } }), vec![], true);

    let plain = ws.evaluate(&req).expect("evaluate must succeed");
    let output: serde_json::Value = plain.output.into();
    assert_eq!(output.pointer("/customer/out"), Some(&json!("second")));

    let enhanced = ws.enhance_trace(&req).expect(
        "enhance_trace must tolerate an error in a column evaluate would short-circuit past",
    );
    let trace = enhanced.trace.expect("trace populated");
    let dt = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt")
        .expect("dt missing from trace");
    let trace_json = serde_json::to_value(&dt.trace).unwrap();
    assert_eq!(trace_json.pointer("/matchedRows"), Some(&json!([1])));
}

#[test]
fn enhance_trace_tolerates_error_after_failed_column_in_collect_table() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "flag", "type": "string", "array": false, "optional": false }
                ]
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "hitPolicy": "collect",
                "inputs": [
                    { "id": "i1", "name": "", "field": "customer.flag" },
                    { "id": "i2", "name": "", "field": "customer.flag" }
                ],
                "outputs": [ { "id": "o1", "name": "", "field": "customer.out" } ],
                "rules": [
                    { "i1": "== 'x'", "i2": "number($) > 2", "o1": "\"first\"" },
                    { "i1": "== ''", "o1": "\"second\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "flag": "" } }), vec![], true);

    let plain = ws.evaluate(&req).expect("evaluate must succeed");
    let output: serde_json::Value = plain.output.into();
    assert_eq!(output.pointer("/customer/out"), Some(&json!(["second"])));

    let enhanced = ws
        .enhance_trace(&req)
        .expect("enhance_trace must tolerate post-short-circuit errors in collect tables");
    let trace = enhanced.trace.expect("trace populated");
    let dt = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt")
        .expect("dt missing from trace");
    let trace_json = serde_json::to_value(&dt.trace).unwrap();
    assert_eq!(trace_json.pointer("/matchedRows"), Some(&json!([1])));
}

#[test]
fn iterated_write_named_like_owner_entity_survives_write_back() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            } } },
            { "id": "dm-company", "type": "dataModel", "props": { "data": {
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "name", "type": "string", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-owner-name", "type": "expression", "props": { "data": {
                "key": "company.customer",
                "value": "company.name + '!'"
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(
        json!({ "customer": { "companies": [ { "name": "Acme" }, { "name": "Mega" } ] } }),
        vec![],
        false,
    );
    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(
        output.pointer("/customer/companies/0/customer"),
        Some(&json!("Acme!")),
        "write named like the owner entity must survive write_back: {output:#?}",
    );
    assert_eq!(
        output.pointer("/customer/companies/1/customer"),
        Some(&json!("Mega!"))
    );
}

#[test]
fn cyclic_property_demand_terminates() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "seed", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-a", "type": "expression", "props": { "data": {
                "key": "customer.a",
                "value": "customer.b + 1"
            } } },
            { "id": "e-b", "type": "expression", "props": { "data": {
                "key": "customer.b",
                "value": "customer.a + 1"
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "seed": 1 } }), vec![], false);
    let _ = ws.evaluate(&req);
}

fn writes_reads_doc() -> serde_json::Value {
    json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "score", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-base", "type": "expression", "props": { "data": {
                "key": "customer.base",
                "value": "customer.score * 2"
            } } },
            { "id": "a-rich", "type": "assertion", "props": { "data": {
                "output": "customer.rich",
                "conditions": [
                    { "id": "c1", "expression": "customer.base >= 30", "operator": "and", "depth": 0 }
                ]
            } } },
            { "id": "m-tier", "type": "match", "props": { "data": {
                "key": "customer.tier",
                "arms": [
                    { "id": "hi", "condition": "customer.rich", "value": "\"gold\"" },
                    { "id": "lo", "condition": "", "value": "\"basic\"" }
                ]
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "hitPolicy": "first",
                "inputs": [
                    { "id": "i1", "name": "", "field": "customer.tier" }
                ],
                "outputs": [
                    { "id": "o1", "name": "", "field": "customer.discount" }
                ],
                "rules": [
                    { "i1": "\"gold\"", "o1": "customer.base * 0.01" },
                    { "i1": "", "o1": "0" }
                ]
            } } }
        ]
    })
}

fn writes_of(ex: &zen_engine::policy::BlockExecution) -> Vec<(String, serde_json::Value)> {
    ex.writes
        .iter()
        .map(|w| (w.path.to_string(), w.value.clone().into()))
        .collect()
}

fn reads_of(ex: &zen_engine::policy::BlockExecution) -> Vec<String> {
    ex.reads.iter().map(|r| r.to_string()).collect()
}

#[test]
fn trace_records_writes_and_reads_per_execution() {
    let ws = workspace_with(writes_reads_doc());
    let req = request(json!({ "customer": { "score": 20 } }), vec![], true);
    let result = ws.enhance_trace(&req).expect("evaluate succeeded");
    let trace = result.trace.expect("trace populated");

    let by_id = |id: &str| {
        trace
            .executions
            .iter()
            .find(|e| e.block_id.as_ref() == id)
            .unwrap_or_else(|| panic!("{id} missing from trace"))
    };

    let expression = by_id("e-base");
    assert_eq!(
        writes_of(expression),
        vec![("customer.base".to_string(), json!(40))]
    );
    assert_eq!(reads_of(expression), vec!["customer.score"]);

    let assertion = by_id("a-rich");
    assert_eq!(
        writes_of(assertion),
        vec![("customer.rich".to_string(), json!(true))]
    );
    assert_eq!(reads_of(assertion), vec!["customer.base"]);

    let match_block = by_id("m-tier");
    assert_eq!(
        writes_of(match_block),
        vec![("customer.tier".to_string(), json!("gold"))]
    );
    assert_eq!(reads_of(match_block), vec!["customer.rich"]);

    let table = by_id("dt");
    assert_eq!(
        writes_of(table),
        vec![("customer.discount".to_string(), json!(0.4))]
    );
    assert_eq!(reads_of(table), vec!["customer.base", "customer.tier"]);
}

#[test]
fn collect_table_trace_records_final_array_write() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "score", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "dt-collect", "type": "decisionTable", "props": { "data": {
                "hitPolicy": "collect",
                "inputs": [
                    { "id": "i1", "name": "", "field": "customer.score" }
                ],
                "outputs": [
                    { "id": "o1", "name": "", "field": "customer.flags" }
                ],
                "rules": [
                    { "i1": ">= 10", "o1": "\"big\"" },
                    { "i1": ">= 0", "o1": "\"pos\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "score": 20 } }), vec![], true);
    let result = ws.enhance_trace(&req).expect("evaluate succeeded");
    let trace = result.trace.expect("trace populated");

    let table = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt-collect")
        .expect("dt-collect missing from trace");
    assert_eq!(
        writes_of(table),
        vec![("customer.flags".to_string(), json!(["big", "pos"]))]
    );
}

#[test]
fn iterated_trace_records_writes_and_reads_per_instance() {
    let doc = json!({
        "blocks": [
            { "id": "dm-customer", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
                ]
            } } },
            { "id": "dm-company", "type": "dataModel", "props": { "data": {
                "name": "company",
                "properties": [
                    { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-rate", "type": "expression", "props": { "data": {
                "key": "customer.rate",
                "value": "2"
            } } },
            { "id": "m-bonus", "type": "match", "props": { "data": {
                "key": "company.bonus",
                "arms": [
                    { "id": "hi", "condition": "company.revenue >= 100", "value": "company.revenue * customer.rate" },
                    { "id": "lo", "condition": "", "value": "0" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(
        json!({ "customer": { "companies": [ { "revenue": 200 }, { "revenue": 50 } ] } }),
        vec![],
        true,
    );
    let result = ws.enhance_trace(&req).expect("evaluate succeeded");
    let trace = result.trace.expect("trace populated");

    let instances: Vec<_> = trace
        .executions
        .iter()
        .filter(|e| e.block_id.as_ref() == "m-bonus")
        .collect();
    assert_eq!(instances.len(), 2);

    assert_eq!(
        instances[0].instance_path.as_deref(),
        Some("customer.companies.0")
    );
    assert_eq!(
        writes_of(instances[0]),
        vec![("company.bonus".to_string(), json!(400))]
    );
    assert_eq!(
        reads_of(instances[0]),
        vec!["company.revenue", "customer.rate"]
    );

    assert_eq!(
        instances[1].instance_path.as_deref(),
        Some("customer.companies.1")
    );
    assert_eq!(
        writes_of(instances[1]),
        vec![("company.bonus".to_string(), json!(0))]
    );
    assert_eq!(reads_of(instances[1]), vec!["company.revenue"]);
}

fn nested_totals_doc() -> serde_json::Value {
    json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "base", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-ta", "type": "expression", "props": { "data": {
                "key": "totals.a",
                "value": "customer.base * 2"
            } } },
            { "id": "e-tb", "type": "expression", "props": { "data": {
                "key": "totals.b",
                "value": "customer.base * 3"
            } } }
        ]
    })
}

#[test]
fn sibling_writers_under_shared_nested_root_all_execute() {
    let ws = workspace_with(nested_totals_doc());
    let req = request(json!({ "customer": { "base": 10 } }), vec![], true);
    let executed = executed_block_ids(&ws, &req);

    assert!(
        executed.contains(&"e-ta".to_string()),
        "first sibling writer must run: {executed:?}"
    );
    assert!(
        executed.contains(&"e-tb".to_string()),
        "second sibling writer must run: {executed:?}"
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/totals/a"), Some(&json!(20)));
    assert_eq!(output.pointer("/totals/b"), Some(&json!(30)));
}

#[test]
fn goal_on_shared_nested_root_runs_all_sibling_writers() {
    let ws = workspace_with(nested_totals_doc());
    let req = request(json!({ "customer": { "base": 10 } }), vec!["totals"], true);
    let executed = executed_block_ids(&ws, &req);

    assert!(executed.contains(&"e-ta".to_string()));
    assert!(
        executed.contains(&"e-tb".to_string()),
        "ancestor goal must fan out to every descendant writer: {executed:?}"
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/totals/a"), Some(&json!(20)));
    assert_eq!(output.pointer("/totals/b"), Some(&json!(30)));
}

#[test]
fn entity_rooted_sibling_writers_under_shared_nested_root_all_execute() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "base", "type": "number", "array": false, "optional": false }
                ]
            } } },
            { "id": "e-ta", "type": "expression", "props": { "data": {
                "key": "customer.totals.a",
                "value": "customer.base * 2"
            } } },
            { "id": "e-tb", "type": "expression", "props": { "data": {
                "key": "customer.totals.b",
                "value": "customer.base * 3"
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let req = request(json!({ "customer": { "base": 10 } }), vec![], true);
    let executed = executed_block_ids(&ws, &req);

    assert!(executed.contains(&"e-ta".to_string()));
    assert!(
        executed.contains(&"e-tb".to_string()),
        "entity-rooted sibling writer must run: {executed:?}"
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/customer/totals/a"), Some(&json!(20)));
    assert_eq!(output.pointer("/customer/totals/b"), Some(&json!(30)));
}

#[test]
fn goal_on_nested_leaf_prunes_sibling_writer() {
    let ws = workspace_with(nested_totals_doc());
    let req = request(
        json!({ "customer": { "base": 10 } }),
        vec!["totals.a"],
        true,
    );
    let executed = executed_block_ids(&ws, &req);

    assert!(executed.contains(&"e-ta".to_string()));
    assert!(
        !executed.contains(&"e-tb".to_string()),
        "leaf goal must stay lazy and skip the sibling writer: {executed:?}"
    );

    let result = ws.evaluate(&req).expect("evaluate succeeded");
    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/totals/a"), Some(&json!(20)));
    assert_eq!(output.pointer("/totals/b"), None);
}

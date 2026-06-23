use serde_json::json;
use std::sync::Arc;
use zen_engine::policy::{PolicyWorkspace, ReferenceKind, RenameTarget, ScopeRequest};

fn workspace_with(doc: serde_json::Value) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws
}

fn customer_company_dm() -> Vec<serde_json::Value> {
    vec![
        json!({ "id": "dm-customer", "type": "dataModel", "props": { "data": {
            "name": "customer",
            "properties": [
                { "id": "p1", "name": "name", "type": "string", "array": false, "optional": false },
                { "id": "p2", "name": "companies", "type": "relationship", "target": "company", "array": true, "optional": false }
            ]
        } } }),
        json!({ "id": "dm-company", "type": "dataModel", "props": { "data": {
            "name": "company",
            "properties": [
                { "id": "p1", "name": "name", "type": "string", "array": false, "optional": false },
                { "id": "p2", "name": "revenue", "type": "number", "array": false, "optional": false }
            ]
        } } }),
    ]
}

fn instance_of(ws: &PolicyWorkspace, entity: &str, field: &str) -> Option<serde_json::Value> {
    let entities = ws.entities(&ScopeRequest::for_policy("p"));
    let entity_obj = entities.iter().find(|e| e.name.as_ref() == entity)?;
    let field_obj = entity_obj
        .fields
        .iter()
        .find(|f| f.name.as_ref() == field)?;
    let origin = serde_json::to_value(&field_obj.origin).unwrap();
    origin.get("instanceOf").cloned()
}

#[test]
fn chained_computed_identity_resolves_transitively() {
    let mut blocks = customer_company_dm();
    blocks.push(
        json!({ "id": "e1", "type": "expression", "props": { "data": {
        "key": "customer.profitable",
        "value": "filter(customer.companies, $.revenue > 0)"
    } } }),
    );
    blocks.push(
        json!({ "id": "e2", "type": "expression", "props": { "data": {
        "key": "customer.topThree",
        "value": "customer.profitable[0:3]"
    } } }),
    );
    blocks.push(
        json!({ "id": "e3", "type": "expression", "props": { "data": {
        "key": "customer.best",
        "value": "customer.topThree[0]"
    } } }),
    );
    let ws = workspace_with(json!({ "blocks": blocks }));

    assert_eq!(
        instance_of(&ws, "customer", "profitable"),
        Some(json!({ "target": "company", "array": true }))
    );
    assert_eq!(
        instance_of(&ws, "customer", "topThree"),
        Some(json!({ "target": "company", "array": true }))
    );
    assert_eq!(
        instance_of(&ws, "customer", "best"),
        Some(json!({ "target": "company", "array": false }))
    );
}

#[test]
fn match_arms_with_agreeing_identity() {
    let mut blocks = customer_company_dm();
    blocks.push(json!({ "id": "m1", "type": "match", "props": { "data": {
        "key": "customer.picked",
        "arms": [
            { "id": "a1", "condition": "customer.name == \"vip\"", "value": "customer.companies[0]" },
            { "id": "a2", "condition": "", "value": "customer.companies[1]" }
        ]
    } } }));
    let ws = workspace_with(json!({ "blocks": blocks }));
    assert_eq!(
        instance_of(&ws, "customer", "picked"),
        Some(json!({ "target": "company", "array": false }))
    );
}

#[test]
fn match_arms_with_disagreeing_identity_erase() {
    let mut blocks = customer_company_dm();
    blocks.push(json!({ "id": "m1", "type": "match", "props": { "data": {
        "key": "customer.picked",
        "arms": [
            { "id": "a1", "condition": "customer.name == \"vip\"", "value": "customer.companies[0]" },
            { "id": "a2", "condition": "", "value": "customer.name" }
        ]
    } } }));
    let ws = workspace_with(json!({ "blocks": blocks }));
    assert_eq!(instance_of(&ws, "customer", "picked"), None);
}

#[test]
fn pool_root_filter_keeps_array() {
    let doc = json!({ "blocks": [
        { "id": "dm-customer", "type": "dataModel", "props": { "data": {
            "name": "customer",
            "properties": [
                { "id": "p1", "name": "favorite", "type": "reference", "target": "company", "array": false, "optional": false }
            ]
        } } },
        { "id": "dm-company", "type": "dataModel", "props": { "data": {
            "name": "company",
            "properties": [
                { "id": "p1", "name": "revenue", "type": "number", "array": false, "optional": false }
            ]
        } } },
        { "id": "e1", "type": "expression", "props": { "data": {
            "key": "customer.richPool",
            "value": "filter(company, $.revenue > 100)"
        } } }
    ] });
    let ws = workspace_with(doc);
    assert_eq!(
        instance_of(&ws, "customer", "richPool"),
        Some(json!({ "target": "company", "array": true }))
    );
}

#[test]
fn outputs_carry_instance_of() {
    let mut blocks = customer_company_dm();
    blocks.push(
        json!({ "id": "e1", "type": "expression", "props": { "data": {
        "key": "customer.profitable",
        "value": "filter(customer.companies, $.revenue > 0)"
    } } }),
    );
    let ws = workspace_with(json!({ "blocks": blocks }));
    let outputs = ws.outputs(&ScopeRequest::for_policy("p"));
    let profitable = outputs
        .iter()
        .find(|o| o.path.as_ref() == "customer.profitable")
        .expect("output registered");
    let serialized = serde_json::to_value(profitable).unwrap();
    assert_eq!(
        serialized["instanceOf"],
        json!({ "target": "company", "array": true })
    );
}

#[test]
fn rename_finds_reads_through_computed_collection_and_pointer() {
    let mut blocks = customer_company_dm();
    blocks.push(
        json!({ "id": "e1", "type": "expression", "props": { "data": {
        "key": "customer.profitable",
        "value": "filter(customer.companies, $.revenue > 0)"
    } } }),
    );
    blocks.push(
        json!({ "id": "e2", "type": "expression", "props": { "data": {
        "key": "customer.total",
        "value": "sum(map(customer.profitable as c, c.revenue))"
    } } }),
    );
    let ws = workspace_with(json!({ "blocks": blocks }));

    let sites = ws.references(&RenameTarget::Field {
        entity: Arc::from("company"),
        field: Arc::from("revenue"),
    });
    let expression_sites: Vec<&str> = sites
        .iter()
        .filter(|s| s.kind == ReferenceKind::ExpressionRead)
        .map(|s| s.block_id.as_ref())
        .collect();
    assert!(
        expression_sites.contains(&"e1"),
        "$ read over declared relationship must be found: {sites:#?}"
    );
    assert!(
        expression_sites.contains(&"e2"),
        "alias read over computed collection must be found: {sites:#?}"
    );

    let edits = ws.rename(
        &RenameTarget::Field {
            entity: Arc::from("company"),
            field: Arc::from("revenue"),
        },
        "income",
    );
    let serialized = serde_json::to_string(&edits).unwrap();
    assert!(
        serialized.contains("$.income > 0"),
        "pointer read must be rewritten: {serialized}"
    );
    assert!(
        serialized.contains("c.income"),
        "alias read must be rewritten: {serialized}"
    );
}

#[test]
fn map_and_arithmetic_erase_identity() {
    let mut blocks = customer_company_dm();
    blocks.push(
        json!({ "id": "e1", "type": "expression", "props": { "data": {
        "key": "customer.summaries",
        "value": "map(customer.companies as c, { label: c.name })"
    } } }),
    );
    blocks.push(
        json!({ "id": "e2", "type": "expression", "props": { "data": {
        "key": "customer.count",
        "value": "len(customer.companies)"
    } } }),
    );
    let ws = workspace_with(json!({ "blocks": blocks }));
    assert_eq!(instance_of(&ws, "customer", "summaries"), None);
    assert_eq!(instance_of(&ws, "customer", "count"), None);
}

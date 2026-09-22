use serde_json::json;
use std::sync::Arc;
use zen_engine::policy::{EvaluateRequest, EvaluationError, PolicyWorkspace, ScopeRequest};
use zen_expression::variable::Variable;

fn dictionary_block(id: &str, name: &str, entries: &[(&str, &str)]) -> serde_json::Value {
    json!({
        "id": id,
        "type": "dictionary",
        "props": {
            "data": {
                "name": name,
                "entries": entries.iter().enumerate().map(|(i, (value, label))| json!({
                    "id": format!("e{i}"),
                    "value": value,
                    "label": label,
                })).collect::<Vec<_>>(),
            }
        }
    })
}

fn tier_dictionary() -> serde_json::Value {
    dictionary_block(
        "dict1",
        "customerTier",
        &[("VIP", "Very important"), ("STD", "Standard")],
    )
}

fn expression_block(id: &str, key: &str, value: &str) -> serde_json::Value {
    json!({
        "id": id,
        "type": "expression",
        "props": { "data": { "key": key, "value": value } }
    })
}

fn workspace_with(blocks: Vec<serde_json::Value>) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy(
        "main",
        serde_json::from_value(json!({ "blocks": blocks })).unwrap(),
    );
    ws
}

fn evaluate(
    ws: &PolicyWorkspace,
    input: serde_json::Value,
) -> Result<serde_json::Value, EvaluationError> {
    let result = ws.evaluate(&EvaluateRequest {
        policy_path: Arc::from("main"),
        input: Variable::from(input),
        goals: Vec::new(),
        trace: false,
    })?;
    Ok(result.output.to_value())
}

#[test]
fn dictionary_is_not_referenceable_in_expressions() {
    for member in ["customerTier.VIP", "customerTier.GOLD"] {
        let ws = workspace_with(vec![
            tier_dictionary(),
            expression_block("e1", "tier", member),
        ]);

        let diagnostics = ws.diagnostics("main");
        assert!(
            diagnostics
                .iter()
                .any(|d| format!("{d:?}").contains("customerTier")),
            "expected '{member}' to be an unknown property, got: {diagnostics:?}"
        );
    }
}

#[test]
fn dictionary_typed_field_compares_as_plain_string() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        json!({
            "id": "dm1",
            "type": "dataModel",
            "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "tier", "type": "relationship", "target": "customerTier", "array": false, "optional": false }
                ]
            }}
        }),
        expression_block("e1", "customer.isVip", "customer.tier == 'VIP'"),
    ]);

    let diagnostics = ws.diagnostics("main");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let output = evaluate(&ws, json!({ "customer": { "tier": "VIP" } })).unwrap();
    assert_eq!(output["customer"]["isVip"], json!(true));
}

#[test]
fn duplicate_values_are_diagnosed() {
    let ws = workspace_with(vec![dictionary_block(
        "dict1",
        "customerTier",
        &[("VIP", "One"), ("VIP", "Two")],
    )]);

    let diagnostics = ws.diagnostics("main");
    assert!(
        diagnostics
            .iter()
            .any(|d| format!("{d:?}").contains("duplicate value 'VIP'")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn dictionary_name_collisions_are_diagnosed() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        dictionary_block("dict2", "customerTier", &[("A", "")]),
    ]);
    let diagnostics = ws.diagnostics("main");
    assert!(
        diagnostics
            .iter()
            .any(|d| format!("{d:?}").contains("already defined")),
        "got: {diagnostics:?}"
    );

    let ws = workspace_with(vec![
        tier_dictionary(),
        json!({
            "id": "dm1",
            "type": "dataModel",
            "props": { "data": {
                "name": "customerTier",
                "properties": [
                    { "id": "p1", "name": "age", "type": "number", "array": false, "optional": false }
                ]
            }}
        }),
    ]);
    let diagnostics = ws.diagnostics("main");
    assert!(
        diagnostics
            .iter()
            .any(|d| format!("{d:?}").contains("collides with an entity")),
        "got: {diagnostics:?}"
    );
}

#[test]
fn data_model_property_can_reference_dictionary() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        json!({
            "id": "dm1",
            "type": "dataModel",
            "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "tier", "type": "relationship", "target": "customerTier", "array": false, "optional": false }
                ]
            }}
        }),
        expression_block("e1", "customer.isVip", "customer.tier == 'VIP'"),
    ]);

    let diagnostics = ws.diagnostics("main");
    assert!(
        !diagnostics
            .iter()
            .any(|d| format!("{d:?}").contains("unknown entity")),
        "dictionary target must not be an unknown entity: {diagnostics:?}"
    );

    let output = evaluate(&ws, json!({ "customer": { "tier": "VIP" } })).unwrap();
    assert_eq!(output["customer"]["isVip"], json!(true));

    let invalid = evaluate(&ws, json!({ "customer": { "tier": "GOLD" } }));
    assert!(
        matches!(invalid, Err(EvaluationError::InputValidationFailed { .. })),
        "value outside the dictionary must fail input validation"
    );
}

#[test]
fn dictionary_membership_is_validated_for_arrays_and_globals() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        json!({
            "id": "dm1",
            "type": "dataModel",
            "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "tiers", "type": "relationship", "target": "customerTier", "array": true, "optional": false }
                ]
            }}
        }),
        json!({
            "id": "dm2",
            "type": "dataModel",
            "props": { "data": {
                "name": "global",
                "scope": "global",
                "properties": [
                    { "id": "p2", "name": "defaultTier", "type": "relationship", "target": "customerTier", "array": false, "optional": true }
                ]
            }}
        }),
        expression_block("e1", "customer.first", "customer.tiers[0]"),
    ]);

    let ok = evaluate(
        &ws,
        json!({ "customer": { "tiers": ["VIP", "STD"] }, "defaultTier": "STD" }),
    )
    .unwrap();
    assert_eq!(ok["customer"]["first"], json!("VIP"));

    let bad_element = evaluate(&ws, json!({ "customer": { "tiers": ["VIP", "GOLD"] } }));
    assert!(matches!(
        bad_element,
        Err(EvaluationError::InputValidationFailed { .. })
    ));

    let bad_global = evaluate(
        &ws,
        json!({ "customer": { "tiers": [] }, "defaultTier": "GOLD" }),
    );
    assert!(matches!(
        bad_global,
        Err(EvaluationError::InputValidationFailed { .. })
    ));

    let not_a_string = evaluate(&ws, json!({ "customer": { "tiers": [42] } }));
    assert!(matches!(
        not_a_string,
        Err(EvaluationError::InputValidationFailed { .. })
    ));
}

#[test]
fn input_key_matching_dictionary_name_is_plain_data() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        expression_block("e1", "echo", "input"),
    ]);

    let output = evaluate(&ws, json!({ "input": 1, "customerTier": "boom" })).unwrap();
    assert_eq!(output["echo"], json!(1));
    assert_eq!(output["customerTier"], json!("boom"));
}

#[test]
fn dictionaries_query_exposes_labels() {
    let ws = workspace_with(vec![tier_dictionary()]);
    let dictionaries = ws.dictionaries(&ScopeRequest {
        policy_path: Arc::from("main"),
        goals: Vec::new(),
    });

    assert_eq!(dictionaries.len(), 1);
    let dict = &dictionaries[0];
    assert_eq!(dict.name.as_ref(), "customerTier");
    assert_eq!(dict.entries.len(), 2);
    assert_eq!(dict.entries[0].value.as_ref(), "VIP");
    assert_eq!(dict.entries[0].label.as_ref(), "Very important");
}

#[test]
fn dictionary_block_round_trips_through_wire_format() {
    let block_json = tier_dictionary();
    let block: zen_engine::policy::BlockDoc = serde_json::from_value(block_json.clone()).unwrap();
    let serialized = serde_json::to_value(&block).unwrap();
    assert_eq!(serialized, block_json);
}

#[test]
fn non_member_literal_comparison_is_diagnosed() {
    let ws = workspace_with(vec![
        tier_dictionary(),
        json!({
            "id": "dm1",
            "type": "dataModel",
            "props": { "data": {
                "name": "customer",
                "properties": [
                    { "id": "p1", "name": "tier", "type": "relationship", "target": "customerTier", "array": false, "optional": false }
                ]
            }}
        }),
        expression_block("e1", "customer.flag", "customer.tier == 'GOLD'"),
    ]);

    let diagnostics = ws.diagnostics("main");
    assert!(
        diagnostics
            .iter()
            .any(|d| format!("{d:?}").contains("GOLD")),
        "expected non-member literal to be diagnosed, got: {diagnostics:?}"
    );
}

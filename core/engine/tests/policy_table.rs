use serde_json::json;
use std::sync::Arc;
use zen_engine::policy::{
    EngineEdit, EvaluateRequest, PolicyWorkspace, RenameTarget, ScopeRequest, Severity,
};
use zen_expression::variable::{Variable, VariableType};

fn workspace_with(doc: serde_json::Value) -> PolicyWorkspace {
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(doc).unwrap());
    ws
}

fn request(input: serde_json::Value, trace: bool) -> EvaluateRequest {
    EvaluateRequest {
        policy_path: Arc::from("p"),
        input: Variable::from(input),
        goals: Vec::new(),
        trace,
    }
}

fn evaluate_output(ws: &PolicyWorkspace, input: serde_json::Value) -> serde_json::Value {
    let result = ws.evaluate(&request(input, false)).expect("evaluate");
    result.output.into()
}

fn output_type(ws: &PolicyWorkspace, path: &str) -> VariableType {
    ws.outputs(&ScopeRequest::for_policy("p"))
        .into_iter()
        .find(|o| o.path.as_ref() == path)
        .unwrap_or_else(|| panic!("output {path} not registered"))
        .resolved_type
}

fn error_messages(ws: &PolicyWorkspace) -> Vec<String> {
    ws.diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message.to_string())
        .collect()
}

fn order_dm() -> serde_json::Value {
    json!({ "id": "dm", "type": "dataModel", "props": { "data": {
        "name": "order",
        "properties": [
            { "id": "p1", "name": "amount", "type": "number", "array": false, "optional": false },
            { "id": "p2", "name": "region", "type": "string", "enum": ["US", "EU"], "array": false, "optional": false },
            { "id": "p3", "name": "express", "type": "boolean", "array": false, "optional": false }
        ]
    } } })
}

fn strict_table_doc() -> serde_json::Value {
    json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [
                    { "id": "i1", "name": "", "field": "order.amount" },
                    { "id": "i2", "name": "", "field": "order.region" }
                ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.shippingCost" } ],
                "rules": [
                    { "i1": ">= 100", "i2": "\"US\"", "o1": "0" },
                    { "i1": ">= 100", "i2": "\"EU\"", "o1": "5" }
                ]
            } } }
        ]
    })
}

#[test]
fn no_match_emits_null_for_scalar_output() {
    let ws = workspace_with(strict_table_doc());
    let output = evaluate_output(&ws, json!({ "order": { "amount": 50, "region": "US" } }));
    assert_eq!(
        output.pointer("/order/shippingCost"),
        Some(&serde_json::Value::Null),
        "no-match scalar column must write an explicit null; got {output:?}"
    );
}

#[test]
fn no_match_trace_has_no_matched_rows() {
    let ws = workspace_with(strict_table_doc());
    let result = ws
        .evaluate(&request(
            json!({ "order": { "amount": 50, "region": "US" } }),
            true,
        ))
        .expect("evaluate");
    let trace = result.trace.expect("trace");
    let dt = trace
        .executions
        .iter()
        .find(|e| e.block_id.as_ref() == "dt")
        .expect("dt execution");
    let trace_json = serde_json::to_value(&dt.trace).unwrap();
    assert_eq!(trace_json["matchedRows"], json!([]));
}

#[test]
fn uncovered_scalar_output_is_nullable() {
    let ws = workspace_with(strict_table_doc());
    assert!(error_messages(&ws).is_empty());
    assert!(
        matches!(
            output_type(&ws, "order.shippingCost"),
            VariableType::Nullable(_)
        ),
        "no catch-all and partial coverage must produce a nullable output"
    );
}

#[test]
fn catch_all_row_makes_scalar_output_non_nullable() {
    let mut doc = strict_table_doc();
    doc["blocks"][1]["props"]["data"]["rules"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "i1": "", "i2": "", "o1": "20" }));
    let ws = workspace_with(doc);
    assert!(error_messages(&ws).is_empty());
    assert!(
        matches!(output_type(&ws, "order.shippingCost"), VariableType::Number),
        "catch-all row must prove coverage"
    );
}

#[test]
fn enum_union_across_rows_covers() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.region" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.zone" } ],
                "rules": [
                    { "i1": "\"US\"", "o1": "1" },
                    { "i1": "\"EU\"", "o1": "2" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    assert!(error_messages(&ws).is_empty());
    assert!(matches!(
        output_type(&ws, "order.zone"),
        VariableType::Number
    ));
}

#[test]
fn partial_enum_union_stays_nullable() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.region" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.zone" } ],
                "rules": [ { "i1": "\"US\"", "o1": "1" } ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    assert!(matches!(
        output_type(&ws, "order.zone"),
        VariableType::Nullable(_)
    ));
}

#[test]
fn number_tiling_covers_and_gap_does_not() {
    let tiled = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.band" } ],
                "rules": [
                    { "i1": "< 100", "o1": "\"low\"" },
                    { "i1": "[100..500]", "o1": "\"mid\"" },
                    { "i1": "> 500", "o1": "\"high\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(tiled);
    assert!(
        !matches!(output_type(&ws, "order.band"), VariableType::Nullable(_)),
        "tiled number coverage must be non-nullable"
    );

    let gapped = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.band" } ],
                "rules": [
                    { "i1": "< 100", "o1": "\"low\"" },
                    { "i1": "> 500", "o1": "\"high\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(gapped);
    assert!(matches!(
        output_type(&ws, "order.band"),
        VariableType::Nullable(_)
    ));
}

#[test]
fn bool_both_values_cover() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.express" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.fee" } ],
                "rules": [
                    { "i1": "true", "o1": "10" },
                    { "i1": "false", "o1": "0" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    assert!(matches!(
        output_type(&ws, "order.fee"),
        VariableType::Number
    ));
}

#[test]
fn multi_column_rows_do_not_prove_coverage() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [
                    { "id": "i1", "name": "", "field": "order.region" },
                    { "id": "i2", "name": "", "field": "order.amount" }
                ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.zone" } ],
                "rules": [
                    { "i1": "\"US\"", "i2": "> 0", "o1": "1" },
                    { "i1": "\"EU\"", "i2": "> 0", "o1": "2" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    assert!(matches!(
        output_type(&ws, "order.zone"),
        VariableType::Nullable(_)
    ));
}

#[test]
fn optional_input_field_is_not_covered_without_catch_all() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "order",
                "properties": [
                    { "id": "p2", "name": "region", "type": "string", "enum": ["US", "EU"], "array": false, "optional": true }
                ]
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.region" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.zone" } ],
                "rules": [
                    { "i1": "\"US\"", "o1": "1" },
                    { "i1": "\"EU\"", "o1": "2" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    assert!(matches!(
        output_type(&ws, "order.zone"),
        VariableType::Nullable(_)
    ));
}

fn mixed_table_doc() -> serde_json::Value {
    json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [
                    { "id": "o1", "name": "", "field": "order.tags[]" },
                    { "id": "o2", "name": "", "field": "order.tier" }
                ],
                "rules": [
                    { "i1": "> 1000", "o1": "\"vip\"", "o2": "" },
                    { "i1": "> 100", "o1": "\"bulk\"", "o2": "\"gold\"" },
                    { "i1": "> 10", "o1": "", "o2": "\"silver\"" },
                    { "i1": "", "o1": "\"standard\"", "o2": "\"bronze\"" }
                ]
            } } }
        ]
    })
}

#[test]
fn mixed_table_collects_tags_and_first_matches_tier() {
    let ws = workspace_with(mixed_table_doc());
    assert!(error_messages(&ws).is_empty(), "{:?}", error_messages(&ws));

    let output = evaluate_output(
        &ws,
        json!({ "order": { "amount": 5000, "region": "US", "express": false } }),
    );
    assert_eq!(
        output.pointer("/order/tags"),
        Some(&json!(["vip", "bulk", "standard"]))
    );
    assert_eq!(output.pointer("/order/tier"), Some(&json!("gold")));

    let output = evaluate_output(
        &ws,
        json!({ "order": { "amount": 50, "region": "US", "express": false } }),
    );
    assert_eq!(output.pointer("/order/tags"), Some(&json!(["standard"])));
    assert_eq!(output.pointer("/order/tier"), Some(&json!("silver")));
}

#[test]
fn scalar_falls_through_rows_with_empty_cell() {
    let ws = workspace_with(mixed_table_doc());
    let output = evaluate_output(
        &ws,
        json!({ "order": { "amount": 5000, "region": "US", "express": false } }),
    );
    assert_eq!(
        output.pointer("/order/tier"),
        Some(&json!("gold")),
        "row 0 matches with an empty tier cell; tier must fall through to row 1"
    );
}

#[test]
fn mixed_table_types_are_array_and_covered_scalar() {
    let ws = workspace_with(mixed_table_doc());
    let tags = output_type(&ws, "order.tags");
    assert!(
        matches!(&tags, VariableType::Array(inner) if !matches!(inner.as_ref(), VariableType::Nullable(_))),
        "collect column must type as a clean array, got {tags}"
    );
    let tier = output_type(&ws, "order.tier");
    assert!(
        !matches!(tier, VariableType::Nullable(_)),
        "catch-all row provides tier coverage, got {tier}"
    );
    assert_eq!(format!("{tier}"), "\"gold\" | \"silver\" | \"bronze\"");
}

#[test]
fn no_match_collect_emits_empty_array() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.tags[]" } ],
                "rules": [ { "i1": "> 1000", "o1": "\"vip\"" } ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let output = evaluate_output(
        &ws,
        json!({ "order": { "amount": 5, "region": "US", "express": false } }),
    );
    assert_eq!(output.pointer("/order/tags"), Some(&json!([])));
}

#[test]
fn legacy_collect_hit_policy_marks_all_columns() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "hitPolicy": "collect",
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.tags" } ],
                "rules": [
                    { "i1": "> 10", "o1": "\"a\"" },
                    { "i1": "> 100", "o1": "\"b\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    assert!(matches!(
        output_type(&ws, "order.tags"),
        VariableType::Array(_)
    ));
    let output = evaluate_output(
        &ws,
        json!({ "order": { "amount": 500, "region": "US", "express": false } }),
    );
    assert_eq!(output.pointer("/order/tags"), Some(&json!(["a", "b"])));
}

#[test]
fn collect_marker_mid_path_errors() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.tags[].x" } ],
                "rules": [ { "i1": "", "o1": "\"a\"" } ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let errors = error_messages(&ws);
    assert!(
        errors.iter().any(|m| m.contains("[]")),
        "mid-path [] must raise InvalidWritePath, got {errors:?}"
    );
}

#[test]
fn bare_collect_marker_errors() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [ { "id": "o1", "name": "", "field": "[]" } ],
                "rules": [ { "i1": "", "o1": "\"a\"" } ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let errors = error_messages(&ws);
    assert!(
        !errors.is_empty(),
        "bare [] field must raise InvalidWritePath"
    );
}

#[test]
fn rename_rewrites_collect_field_keeping_marker() {
    let ws = workspace_with(mixed_table_doc());
    let edits = ws.rename(
        &RenameTarget::Field {
            entity: Arc::from("order"),
            field: Arc::from("tags"),
        },
        "labels",
    );
    let rewritten = edits
        .iter()
        .find_map(|e| match e {
            EngineEdit::ReplaceBlock { new_block, .. } => {
                Some(serde_json::to_string(new_block).unwrap())
            }
            _ => None,
        })
        .expect("rename must rewrite the table block");
    assert!(
        rewritten.contains("order.labels[]"),
        "collect marker must survive rename, got {rewritten}"
    );
}

#[test]
fn unused_scalar_cells_stay_lazy() {
    let doc = json!({
        "blocks": [
            order_dm(),
            { "id": "e-gold", "type": "expression", "props": { "data": {
                "key": "order.goldLabel",
                "value": "\"gold\""
            } } },
            { "id": "e-silver", "type": "expression", "props": { "data": {
                "key": "order.silverLabel",
                "value": "\"silver\""
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [ { "id": "i1", "name": "", "field": "order.amount" } ],
                "outputs": [
                    { "id": "o1", "name": "", "field": "order.tags[]" },
                    { "id": "o2", "name": "", "field": "order.tier" }
                ],
                "rules": [
                    { "i1": "> 100", "o1": "\"bulk\"", "o2": "order.goldLabel" },
                    { "i1": "", "o1": "\"standard\"", "o2": "order.silverLabel" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let result = ws
        .evaluate(&request(
            json!({ "order": { "amount": 500, "region": "US", "express": false } }),
            true,
        ))
        .expect("evaluate");
    let executed: Vec<String> = result
        .trace
        .expect("trace")
        .executions
        .iter()
        .map(|e| e.block_id.to_string())
        .collect();
    assert!(
        executed.contains(&"e-gold".to_string()),
        "resolving row's tier dependency must run: {executed:?}"
    );
    assert!(
        !executed.contains(&"e-silver".to_string()),
        "non-resolving row's tier dependency must stay lazy: {executed:?}"
    );

    let output: serde_json::Value = result.output.into();
    assert_eq!(output.pointer("/order/tier"), Some(&json!("gold")));
    assert_eq!(
        output.pointer("/order/tags"),
        Some(&json!(["bulk", "standard"]))
    );
}

#[test]
fn equality_index_matches_linear_semantics() {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "order",
                "properties": [
                    { "id": "p1", "name": "amount", "type": "number", "array": false, "optional": false },
                    { "id": "p2", "name": "region", "type": "string", "array": false, "optional": false },
                    { "id": "p3", "name": "express", "type": "boolean", "array": false, "optional": false }
                ]
            } } },
            { "id": "dt", "type": "decisionTable", "props": { "data": {
                "inputs": [
                    { "id": "i1", "name": "", "field": "order.region" },
                    { "id": "i2", "name": "", "field": "order.amount" },
                    { "id": "i3", "name": "", "field": "order.express" }
                ],
                "outputs": [ { "id": "o1", "name": "", "field": "order.bucket" } ],
                "rules": [
                    { "i1": "\"US\"", "i2": "1", "i3": "true", "o1": "\"r0\"" },
                    { "i1": "\"US\", \"EU\"", "i2": "2", "i3": "", "o1": "\"r1\"" },
                    { "i1": "in [\"EU\"]", "i2": "in [3, 4]", "i3": "false", "o1": "\"r2\"" },
                    { "i1": "(\"US\")", "i2": "[10..20]", "i3": "", "o1": "\"r3\"" },
                    { "i1": "", "i2": "100", "i3": "", "o1": "\"r4\"" },
                    { "i1": "\"AP\"", "i2": "", "i3": "", "o1": "\"r5\"" },
                    { "i1": "startsWith($, \"E\")", "i2": "777", "i3": "", "o1": "\"r6\"" },
                    { "i1": "\"US\"", "i2": "> 1000", "i3": "", "o1": "\"r7\"" },
                    { "i1": "\"EU\"", "i2": "2.50", "i3": "", "o1": "\"r8\"" },
                    { "i1": "", "i2": "", "i3": "", "o1": "\"r9\"" }
                ]
            } } }
        ]
    });
    let ws = workspace_with(doc);
    let bucket_for =
        |region: &str, amount: serde_json::Value, express: bool| -> serde_json::Value {
            evaluate_output(
                &ws,
                json!({ "order": { "region": region, "amount": amount, "express": express } }),
            )
            .pointer("/order/bucket")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
        };

    assert_eq!(bucket_for("US", json!(1), true), json!("r0"));
    assert_eq!(bucket_for("US", json!(1), false), json!("r9"));
    assert_eq!(bucket_for("EU", json!(2), false), json!("r1"));
    assert_eq!(bucket_for("EU", json!(3), false), json!("r2"));
    assert_eq!(bucket_for("EU", json!(4), true), json!("r9"));
    assert_eq!(bucket_for("US", json!(15), false), json!("r3"));
    assert_eq!(bucket_for("AP", json!(100), false), json!("r4"));
    assert_eq!(bucket_for("AP", json!(7), false), json!("r5"));
    assert_eq!(bucket_for("EU", json!(777), false), json!("r6"));
    assert_eq!(bucket_for("US", json!(5000), false), json!("r7"));
    assert_eq!(bucket_for("EU", json!(2.5), false), json!("r8"));
    assert_eq!(bucket_for("US", json!(999), false), json!("r9"));
}

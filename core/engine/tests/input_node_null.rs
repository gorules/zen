use serde_json::json;
use std::sync::Arc;
use zen_engine::Decision;

fn graph(required: &[&str]) -> Decision {
    let schema = json!({
        "type": "object",
        "properties": {
            "aircraft": {
                "type": "object",
                "properties": { "apuFault": { "type": "boolean" }, "tail": { "type": "string" } },
                "required": required
            }
        },
        "required": ["aircraft"]
    });
    let content = serde_json::from_value(json!({
        "nodes": [
            {
                "id": "in", "name": "in", "type": "inputNode",
                "content": { "schema": schema.to_string() }
            },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [{ "id": "e1", "sourceId": "in", "targetId": "out" }]
    }))
    .unwrap();
    Decision::from(Arc::new(content))
}

#[tokio::test]
async fn null_on_optional_property_reads_as_absent() {
    let decision = graph(&[]);
    let result = decision
        .evaluate(json!({ "aircraft": { "apuFault": null, "tail": "G-EZAA" } }).into())
        .await
        .unwrap();
    assert_eq!(
        result.result,
        json!({ "aircraft": { "apuFault": null, "tail": "G-EZAA" } }).into()
    );
}

#[tokio::test]
async fn null_on_required_property_still_fails() {
    let decision = graph(&["apuFault"]);
    let error = decision
        .evaluate(json!({ "aircraft": { "apuFault": null } }).into())
        .await;
    assert!(error.is_err());
}

#[tokio::test]
async fn wrong_type_on_optional_property_still_fails() {
    let decision = graph(&[]);
    let error = decision
        .evaluate(json!({ "aircraft": { "apuFault": "yes" } }).into())
        .await;
    assert!(error.is_err());
}

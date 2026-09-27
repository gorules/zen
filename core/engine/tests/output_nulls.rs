use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::Decision;

async fn run(expression: &str, schema: Value) -> Result<Value, String> {
    let content: zen_engine::model::GraphContent = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            { "id": "e", "name": "e", "type": "expressionNode", "content": {
                "expressions": [{ "id": "x1", "key": "x", "value": expression }]
            }},
            { "id": "out", "name": "out", "type": "outputNode", "content": { "schema": schema.to_string() } }
        ],
        "edges": [
            { "id": "1", "sourceId": "in", "targetId": "e" },
            { "id": "2", "sourceId": "e", "targetId": "out" }
        ]
    }))
    .unwrap();
    Decision::from(Arc::new(content))
        .evaluate(json!({}).into())
        .await
        .map(|r| serde_json::to_value(&r.result).unwrap())
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn optional_output_fields_accept_null() {
    let optional = json!({ "type": "object", "properties": { "x": { "type": "number" } } });
    assert_eq!(run("null", optional.clone()).await.unwrap(), json!({}));
    assert_eq!(run("5", optional.clone()).await.unwrap(), json!({ "x": 5 }));
    assert!(run("'five'", optional).await.is_err());

    let required = json!({
        "type": "object",
        "properties": { "x": { "type": "number" } },
        "required": ["x"]
    });
    assert!(run("null", required).await.is_err());
}

use serde_json::json;
use zen_engine::model::GraphContent;
use zen_engine::Decision;

fn decision() -> Decision {
    let graph: GraphContent = serde_json::from_value(json!({
        "nodes": [
            { "id": "in", "name": "in", "type": "inputNode", "content": {} },
            { "id": "broken", "name": "broken", "type": "expressionNode", "content": {
                "expressions": [{ "id": "e", "key": "y", "value": "1 +" }]
            } },
            { "id": "fine", "name": "fine", "type": "expressionNode", "content": {
                "expressions": [{ "id": "e", "key": "z", "value": "2" }]
            } },
            { "id": "switch", "name": "switch", "type": "switchNode", "content": {
                "hitPolicy": "first",
                "statements": [
                    { "id": "bad", "condition": "x >" },
                    { "id": "rest", "condition": "" }
                ]
            } },
            { "id": "out", "name": "out", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "a", "sourceId": "in", "targetId": "fine" },
            { "id": "b", "sourceId": "fine", "targetId": "switch" },
            { "id": "c", "sourceId": "switch", "targetId": "broken", "sourceHandle": "bad" },
            { "id": "d", "sourceId": "switch", "targetId": "out", "sourceHandle": "rest" },
            { "id": "e", "sourceId": "broken", "targetId": "out" }
        ]
    }))
    .expect("graph");
    let mut decision = Decision::from(graph).compiled_engine();
    decision.compile();
    decision
}

#[test]
fn nodes_that_fail_to_compile_report_why() {
    let decision = decision();
    let mut degraded = zen_engine::diagnostics::degraded(&decision).expect("compiled");
    degraded.sort();
    let nodes: Vec<&str> = degraded.iter().map(|(node, _)| node.as_str()).collect();
    assert_eq!(nodes, ["broken", "switch"]);
    assert!(degraded.iter().all(|(_, why)| !why.is_empty()));
}

#[tokio::test]
async fn an_invalid_switch_condition_never_matches() {
    let decision = decision();
    let response = decision
        .evaluate(json!({ "x": 1 }).into())
        .await
        .expect("evaluates");
    assert_eq!(response.result.to_value(), json!({ "z": 2 }));
}

#[test]
fn engine_and_decision_stay_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<zen_engine::DecisionEngine>();
    check::<Decision>();
}

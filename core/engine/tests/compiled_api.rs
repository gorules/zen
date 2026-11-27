use serde_json::{json, Value};
use std::sync::Arc;
use zen_engine::loader::MemoryLoader;
use zen_engine::model::DecisionContent;
use zen_engine::{Decision, DecisionEngine, EvaluationOptions};
use zen_expression::variable::Variable;

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn graph_node(id: &str, kind: &str, content: Value) -> Value {
    json!({ "id": id, "name": id, "type": kind, "content": content })
}

fn edge(id: &str, source: &str, target: &str) -> Value {
    json!({ "id": id, "sourceId": source, "targetId": target, "sourceHandle": null })
}

fn dictionary_policy(imports: &[&str], name: &str, entries: &[(&str, &str)]) -> Value {
    json!({
        "imports": imports,
        "blocks": [{
            "id": "dict1",
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
        }]
    })
}

fn schema_graph(imports: &[&str], tier_schema: Value, output_schema: Option<Value>) -> Value {
    let schema = json!({
        "type": "object",
        "properties": { "tier": tier_schema },
        "required": ["tier"]
    });
    let mut nodes = vec![
        graph_node("in", "inputNode", json!({ "schema": schema.to_string() })),
        graph_node(
            "calc",
            "expressionNode",
            json!({ "passThrough": true, "expressions": [{ "id": "e1", "key": "vip", "value": "tier == 'VIP'" }] }),
        ),
    ];
    nodes.push(match output_schema {
        Some(out) => graph_node("out", "outputNode", json!({ "schema": out.to_string() })),
        None => graph_node("out", "outputNode", json!({})),
    });
    json!({
        "imports": imports,
        "nodes": nodes,
        "edges": [edge("e1", "in", "calc"), edge("e2", "calc", "out")]
    })
}

fn policy_content_with_threshold(threshold: i64) -> DecisionContent {
    let doc = json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "platform",
                "scope": "global",
                "properties": [
                    { "id": "g1", "name": "amount", "type": "number", "array": false, "optional": false }
                ]
            }}},
            { "id": "assert", "type": "assertion", "props": { "data": {
                "output": "approved",
                "conditions": [
                    { "id": "c1", "expression": format!("amount >= {threshold}"), "operator": "and", "depth": 0 }
                ]
            }}}
        ]
    });
    serde_json::from_value(doc).unwrap()
}

fn input(amount: i64) -> Variable {
    Variable::from(json!({ "amount": amount }))
}

fn approved(result: &Variable) -> Value {
    serde_json::to_value(result).unwrap()["approved"].clone()
}

fn parse_error_policy() -> DecisionContent {
    serde_json::from_value(json!({
        "blocks": [
            { "id": "dm", "type": "dataModel", "props": { "data": {
                "name": "platform", "scope": "global",
                "properties": [
                    { "id": "g1", "name": "amount", "type": "number", "array": false, "optional": false }
                ]
            }}},
            { "id": "a", "type": "assertion", "props": { "data": {
                "output": "x",
                "conditions": [
                    { "id": "c1", "expression": "if (( invalid )) {{", "operator": "and", "depth": 0 }
                ]
            }}}
        ]
    }))
    .unwrap()
}

#[tokio::test]
async fn unresolved_dictionaries_resolve_per_evaluation_like_the_walker() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add(
        "dicts",
        document(dictionary_policy(
            &[],
            "customerTier",
            &[("VIP", "Very important"), ("STD", "Standard")],
        )),
    );
    let tier = json!({ "$dictionary": "customerTier" });
    let unknown =
        json!({ "type": "object", "properties": { "tier": { "$dictionary": "missingTier" } } });
    let graphs = [
        schema_graph(
            &["dicts"],
            tier.clone(),
            Some(json!({ "type": "object", "properties": { "tier": tier.clone() } })),
        ),
        schema_graph(&[], tier.clone(), None),
        schema_graph(&["dicts"], tier.clone(), Some(unknown.clone())),
        schema_graph(&["absent"], tier.clone(), None),
        schema_graph(&["dicts"], json!({ "type": "string" }), Some(unknown)),
    ];
    let inputs = [
        json!({ "tier": "VIP" }),
        json!({ "tier": "GOLD" }),
        json!({}),
        json!({ "tier": 1 }),
    ];
    for graph in graphs {
        let DecisionContent::Graph(content) = document(graph) else {
            panic!("graph content");
        };
        let decision = Decision::from(content)
            .with_loader(loader.clone())
            .compiled_engine();
        let walker = decision.interpreted();
        let contexts: Vec<zen_expression::Variable> =
            inputs.iter().map(|i| i.clone().into()).collect();
        let batch = decision
            .evaluate_batch(&contexts, EvaluationOptions::default())
            .await;
        for (input, batched) in contexts.iter().zip(batch) {
            let want = walker.evaluate(input.clone()).await;
            let got = decision.evaluate(input.clone()).await;
            let describe = |r: &Result<
                zen_engine::DecisionGraphResponse,
                Box<zen_engine::EvaluationError>,
            >| match r {
                Ok(response) => format!("ok {}", response.result.to_value()),
                Err(error) => format!("err {error} {}", serde_json::to_value(&**error).unwrap()),
            };
            assert_eq!(describe(&want), describe(&got), "{input:?}");
            assert_eq!(describe(&want), describe(&batched), "{input:?}");
        }
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn evaluate_batch_leaves_the_callers_contexts_unchanged() {
    let content = json!({
        "nodes": [
            { "id": "in", "name": "input", "type": "inputNode", "content": {} },
            { "id": "calc", "name": "calc", "type": "expressionNode", "content": {
                "passThrough": true, "inputField": null, "outputPath": null, "executionMode": "single",
                "expressions": [
                    { "id": "e1", "key": "copy", "value": "inp" },
                    { "id": "e2", "key": "copy.extra", "value": "1" }
                ]
            } },
            { "id": "out", "name": "output", "type": "outputNode", "content": {} }
        ],
        "edges": [
            { "id": "a", "sourceId": "in", "targetId": "calc", "type": "edge" },
            { "id": "b", "sourceId": "calc", "targetId": "out", "type": "edge" }
        ]
    });
    let content: zen_engine::model::DecisionContent = serde_json::from_value(content).unwrap();
    let zen_engine::model::DecisionContent::Graph(graph) = content else {
        panic!("graph");
    };
    let mut compiled = Decision::from(graph.clone());
    compiled.compile();
    let decisions = vec![
        Decision::from(graph.clone()).compiled_engine(),
        compiled.compiled_engine(),
    ];
    for decision in decisions {
        let contexts: Vec<Variable> = (0..3)
            .map(|v| json!({ "inp": { "v": v } }).into())
            .collect();
        let before: Vec<serde_json::Value> = contexts.iter().map(Variable::to_value).collect();
        let results = decision
            .evaluate_batch(&contexts, EvaluationOptions::default())
            .await;
        let after: Vec<serde_json::Value> = contexts.iter().map(Variable::to_value).collect();
        assert_eq!(before, after);
        assert!(results.iter().all(Result::is_ok));
    }
}

#[tokio::test]
async fn lazy_policies_follow_changed_content() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("policy", policy_content_with_threshold(100));
    let engine = DecisionEngine::default()
        .compiled_engine()
        .with_loader(loader.clone());

    for _ in 0..3 {
        let result = engine.evaluate("policy", input(150)).await.unwrap();
        assert_eq!(approved(&result.result), json!(true));
    }

    loader.add("policy", policy_content_with_threshold(200));
    for _ in 0..3 {
        let result = engine.evaluate("policy", input(150)).await.unwrap();
        assert_eq!(approved(&result.result), json!(false));
    }

    loader.add("policy", parse_error_policy());
    assert!(engine.evaluate("policy", input(150)).await.is_err());

    loader.add("policy", policy_content_with_threshold(100));
    let result = engine.evaluate("policy", input(150)).await.unwrap();
    assert_eq!(approved(&result.result), json!(true));
}

#[tokio::test]
async fn lazy_policies_follow_changed_imports() {
    let base = |threshold: i64| -> DecisionContent {
        serde_json::from_value(json!({
            "imports": [],
            "blocks": [
                { "id": "dm", "type": "dataModel", "props": { "data": {
                    "name": "platform", "scope": "global",
                    "properties": [
                        { "id": "g1", "name": "amount", "type": "number", "array": false, "optional": false }
                    ]
                }}},
                { "id": "qual", "type": "assertion", "props": { "data": {
                    "output": "qualified",
                    "conditions": [
                        { "id": "c1", "expression": format!("amount >= {threshold}"), "operator": "and", "depth": 0 }
                    ]
                }}}
            ]
        }))
        .unwrap()
    };
    let loader = Arc::new(MemoryLoader::default());
    loader.add("base.json", base(100));
    loader.add(
        "main.json",
        serde_json::from_value::<DecisionContent>(json!({
            "imports": ["base.json"],
            "blocks": [
                { "id": "appr", "type": "assertion", "props": { "data": {
                    "output": "approved",
                    "conditions": [
                        { "id": "c1", "expression": "qualified", "operator": "and", "depth": 0 }
                    ]
                }}}
            ]
        }))
        .unwrap(),
    );
    let engine = DecisionEngine::default()
        .compiled_engine()
        .with_loader(loader.clone());
    let result = engine.evaluate("main.json", input(150)).await.unwrap();
    assert_eq!(approved(&result.result), json!(true));

    loader.add("base.json", base(200));
    let result = engine.evaluate("main.json", input(150)).await.unwrap();
    assert_eq!(approved(&result.result), json!(false));
}

fn customer_policy() -> Value {
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
            } } }
        ]
    })
}

fn shared_customer_contexts() -> Vec<Variable> {
    let shared: Variable = json!({ "customer": { "score": 20 } }).into();
    vec![shared.clone(), shared.clone(), shared]
}

fn assert_untouched(contexts: &[Variable], results: Vec<Value>) {
    for context in contexts {
        assert_eq!(context.to_value(), json!({ "customer": { "score": 20 } }));
    }
    assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(results[0]["customer"]["rich"], json!(true));
}

#[tokio::test]
async fn policy_batches_leave_the_callers_contexts_unchanged() {
    let loader = Arc::new(MemoryLoader::default());
    loader.add("policy", document(customer_policy()));
    let lazy = DecisionEngine::default().with_loader(loader.clone());
    let compiled = DecisionEngine::default().with_loader(loader);
    compiled.compile();
    for engine in [lazy, compiled] {
        let contexts = shared_customer_contexts();
        let results = engine
            .evaluate_batch("policy", &contexts, EvaluationOptions::default())
            .await
            .into_iter()
            .map(|r| r.expect("evaluates").result.to_value())
            .collect();
        assert_untouched(&contexts, results);
    }

    let mut workspace = zen_engine::policy::PolicyWorkspace::new();
    workspace.set_policy("policy", serde_json::from_value(customer_policy()).unwrap());
    let contexts = shared_customer_contexts();
    let requests: Vec<zen_engine::policy::EvaluateRequest> = contexts
        .iter()
        .map(|context| zen_engine::policy::EvaluateRequest {
            policy_path: Arc::from("policy"),
            input: context.clone(),
            goals: Vec::new(),
            trace: zen_engine::trace::TraceFormat::Off,
        })
        .collect();
    let results = workspace
        .evaluate_batch(&requests)
        .into_iter()
        .map(|r| r.expect("evaluates").output.to_value())
        .collect();
    assert_untouched(&contexts, results);
}

//! A document as an endpoint: whether it serves one, and the JSON Schemas of
//! its request (as a caller sends it, and as the rules are given it once the
//! host has filled it in) and of its response.

use serde_json::{json, Value};
use zen_engine::model::DecisionContent;
use zen_engine::policy::{EndpointKind, SchemaAudience, ScopeRequest, Workspace};

fn document(value: Value) -> DecisionContent {
    serde_json::from_value(value).expect("valid decision content")
}

fn entity(name: &str, extra: Value, properties: Value) -> Value {
    let mut data = json!({ "name": name, "properties": properties });
    for (key, value) in extra.as_object().unwrap() {
        data[key] = value.clone();
    }
    json!({ "id": format!("dm-{name}"), "type": "dataModel", "props": { "data": data } })
}

fn prop(name: &str, ty: &str) -> Value {
    json!({ "id": name, "name": name, "type": ty })
}

fn link(name: &str, ty: &str, target: &str) -> Value {
    json!({ "id": name, "name": name, "type": ty, "target": target })
}

/// `models/fraud`, data models only: a transaction (events) references its
/// card (reference data with features), an optional merchant (plain, sent
/// by the caller as a pool), a country keyed by an enum code and a
/// card × merchant pair (a composite key); its lines reference products.
fn models() -> Value {
    json!({ "blocks": [
        entity("transaction", json!({ "events": { "id": "txn_id", "time": "at" } }), json!([
            prop("txn_id", "string"),
            prop("at", "date"),
            prop("amount", "number"),
            { "id": "currency", "name": "currency", "type": "string", "default": "GBP" },
            { "id": "amount_x2", "name": "amount_x2", "type": "number", "compute": "amount * 2" },
            link("card", "reference", "card"),
            { "id": "merchant", "name": "merchant", "type": "reference", "target": "merchant", "optional": true },
            link("country", "reference", "country"),
            link("pair", "reference", "card_merchant"),
            { "id": "lines", "name": "lines", "type": "relationship", "target": "line_item", "array": true }
        ])),
        entity("line_item", json!({}), json!([
            prop("qty", "number"),
            link("product", "reference", "product")
        ])),
        entity("card", json!({ "reference": { "validFrom": "valid_from" } }), json!([
            prop("id", "string"),
            prop("valid_from", "date"),
            prop("holder_country", "string"),
            { "id": "txn_count", "name": "txn_count", "type": "number",
              "feature": { "expr": "count(transaction)", "window": ["1h", "7d"] } },
            { "id": "spend_1d", "name": "spend_1d", "type": "number",
              "feature": { "expr": "sum(transaction as t, t.amount)", "window": "1d", "default": 0 } }
        ])),
        entity("merchant", json!({}), json!([prop("id", "string"), prop("name", "string")])),
        entity("country", json!({ "key": "code" }), json!([
            { "id": "code", "name": "code", "type": "string", "enum": ["GB", "US"] },
            prop("risky", "boolean")
        ])),
        entity("card_merchant", json!({ "key": ["card", "merchant"] }), json!([
            link("card", "reference", "card"),
            link("merchant", "reference", "merchant")
        ])),
        entity("product", json!({}), json!([prop("id", "string"), prop("sku", "string")])),
        { "id": "fs", "type": "featureSettings", "props": { "data": {} } }
    ] })
}

fn rules() -> Value {
    json!({
        "imports": ["models/fraud"],
        "blocks": [
            { "id": "flag", "type": "expression", "props": { "data": {
                "key": "transaction.flag",
                "value": "transaction.amount > 100 and (transaction.card.txn_count_1h ?? 0) > 3 and transaction.country.risky"
            } } }
        ]
    })
}

fn graph(imports: &[&str], input: Value, output: Value, expression: &str) -> Value {
    json!({
        "imports": imports,
        "nodes": [
            { "id": "in", "name": "request", "type": "inputNode", "content": input },
            { "id": "ex", "name": "compute", "type": "expressionNode", "content": { "expressions": [
                { "id": "e1", "key": "total", "value": expression }
            ] } },
            { "id": "out", "name": "response", "type": "outputNode", "content": output }
        ],
        "edges": [
            { "id": "a", "sourceId": "in", "targetId": "ex" },
            { "id": "b", "sourceId": "ex", "targetId": "out" }
        ]
    })
}

fn workspace() -> Workspace {
    let mut ws = Workspace::new();
    ws.set_document("models/fraud", document(models()));
    ws.set_document("fraud", document(rules()));
    ws.set_document(
        "screen",
        document(graph(
            &["models/fraud"],
            json!({ "target": "transaction" }),
            json!({}),
            "amount * 2",
        )),
    );
    ws.set_document(
        "pricing",
        document(graph(
            &[],
            json!({}),
            json!({}),
            "cart.price * cart.quantity",
        )),
    );
    ws
}

fn request(ws: &Workspace, path: &str, audience: SchemaAudience) -> Value {
    ws.request_schema(&ScopeRequest::for_policy(path), audience)
}

#[test]
fn endpoints_are_graphs_and_policies_with_rules() {
    let ws = workspace();
    assert_eq!(ws.endpoint("screen"), Some(EndpointKind::Graph));
    assert_eq!(ws.endpoint("pricing"), Some(EndpointKind::Graph));
    assert_eq!(ws.endpoint("fraud"), Some(EndpointKind::Policy));
    assert_eq!(ws.endpoint("models/fraud"), None, "data models only");
    assert_eq!(ws.endpoint("missing"), None);
    assert_eq!(
        serde_json::to_value(EndpointKind::Policy).unwrap(),
        json!("policy")
    );
    assert_eq!(
        serde_json::to_value(SchemaAudience::Evaluated).unwrap(),
        json!("evaluated")
    );
}

#[test]
fn contract_is_what_the_caller_sends() {
    let ws = workspace();
    let schema = request(&ws, "fraud", SchemaAudience::Contract);
    assert_eq!(
        schema["$schema"],
        json!("http://json-schema.org/draft-07/schema#")
    );
    assert!(schema.get("$id").is_none(), "{schema}");

    let transaction = &schema["properties"]["transaction"]["properties"];
    assert_eq!(transaction["amount"], json!({ "type": "number" }));
    assert_eq!(
        transaction["currency"],
        json!({ "type": "string", "default": "GBP" })
    );
    assert!(
        transaction.get("amount_x2").is_none(),
        "a compute is the host's: {schema}"
    );
    assert_eq!(
        transaction["card"],
        json!({ "type": "string", "description": "card id" })
    );
    assert_eq!(
        transaction["merchant"],
        json!({ "anyOf": [{ "type": "string", "description": "merchant id" }, { "type": "null" }] }),
        "an optional reference may be null"
    );
    assert_eq!(
        transaction["country"],
        json!({ "enum": ["GB", "US"], "description": "country id" }),
        "an enum key"
    );
    assert_eq!(
        transaction["pair"],
        json!({ "type": "string", "description": "card_merchant id" }),
        "a composite key is one joined string"
    );
    assert_eq!(
        transaction["lines"]["items"]["properties"]["product"],
        json!({ "type": "string", "description": "product id" })
    );
    assert_eq!(
        transaction["lines"]["items"]["properties"]["qty"],
        json!({ "type": "number" })
    );

    // The card's records come from the store; the merchant's are sent.
    let properties = schema["properties"].as_object().unwrap();
    assert!(!properties.contains_key("card"), "{schema}");
    let merchant = &schema["properties"]["merchant"]["items"];
    assert_eq!(merchant["required"], json!(["id", "name"]));
    assert_eq!(merchant["additionalProperties"], json!(false));
    let required = schema["required"].as_array().unwrap();
    assert!(required.contains(&json!("transaction")), "{schema}");
    assert!(!required.contains(&json!("card")), "{schema}");
}

#[test]
fn evaluated_is_what_the_rules_are_given() {
    let ws = workspace();
    let schema = request(&ws, "fraud", SchemaAudience::Evaluated);
    let transaction = &schema["properties"]["transaction"]["properties"];
    assert_eq!(
        transaction["amount_x2"],
        json!({ "type": "number" }),
        "{schema}"
    );
    assert_eq!(
        transaction["card"],
        json!({ "type": "string", "description": "card id" }),
        "a policy's reference is an id; its record is in the pool"
    );

    // The card's pool, with its features; its records carry the host's
    // own fields beside them.
    let card = &schema["properties"]["card"]["items"];
    for feature in ["txn_count_1h", "txn_count_7d", "spend_1d"] {
        assert!(
            card["properties"].get(feature).is_some(),
            "{feature}: {schema}"
        );
    }
    let required = card["required"].as_array().unwrap();
    assert!(required.contains(&json!("id")), "{card}");
    assert!(required.contains(&json!("holder_country")), "{card}");
    assert!(
        !required.contains(&json!("spend_1d")),
        "a default fills it in"
    );
    assert!(
        !required.contains(&json!("txn_count_1h")),
        "absent when unknown"
    );
    assert!(card.get("additionalProperties").is_none(), "{card}");
    let pair = &schema["properties"]["card_merchant"]["items"]["properties"];
    assert_eq!(
        pair["card"],
        json!({ "type": "string", "description": "card id" })
    );
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("card")),
        "the host's pool may be absent: {schema}"
    );
}

#[test]
fn a_graph_typed_by_an_entity() {
    let ws = workspace();
    let contract = request(&ws, "screen", SchemaAudience::Contract);
    assert_eq!(
        contract["properties"]["card"],
        json!({ "type": "string", "description": "card id" }),
        "{contract}"
    );
    assert_eq!(
        contract["properties"]["lines"]["items"]["properties"]["product"],
        json!({ "type": "string", "description": "product id" })
    );
    assert!(contract["properties"].get("amount_x2").is_none());

    // Evaluated: the reference is the record it names, its features in it.
    let evaluated = request(&ws, "screen", SchemaAudience::Evaluated);
    let card = &evaluated["properties"]["card"];
    assert_eq!(card["type"], json!("object"), "{evaluated}");
    assert!(
        card["properties"].get("txn_count_1h").is_some(),
        "{evaluated}"
    );
    assert_eq!(
        evaluated["properties"]["lines"]["items"]["properties"]["product"]["properties"]["sku"],
        json!({ "type": "string" })
    );
    assert_eq!(
        evaluated["properties"]["amount_x2"],
        json!({ "type": "number" })
    );
}

#[test]
fn untyped_and_declared_graphs() {
    let mut ws = workspace();
    let inferred = request(&ws, "pricing", SchemaAudience::Contract);
    assert_eq!(inferred["required"], json!(["cart"]), "{inferred}");
    assert!(inferred["properties"]["cart"]["properties"]
        .get("price")
        .is_some());
    assert_eq!(request(&ws, "pricing", SchemaAudience::Evaluated), inferred);

    let declared = json!({ "type": "object", "properties": { "cart": { "type": "object", "description": "Cart" } } });
    ws.set_document(
        "declared",
        document(graph(
            &[],
            json!({ "schema": declared.to_string() }),
            json!({ "schema": "{\"type\":\"object\",\"properties\":{\"total\":{\"type\":\"number\",\"minimum\":0}}}" }),
            "cart.price",
        )),
    );
    assert_eq!(request(&ws, "declared", SchemaAudience::Contract), declared);
    assert_eq!(
        ws.response_schema(&ScopeRequest::for_policy("declared"))["properties"]["total"]["minimum"],
        json!(0)
    );
}

#[test]
fn responses_are_what_the_rules_write() {
    let ws = workspace();
    let response = ws.response_schema(&ScopeRequest::for_policy("fraud"));
    assert_eq!(response["required"], json!(["transaction"]), "{response}");
    assert_eq!(
        response["properties"]["transaction"]["properties"]["flag"],
        json!({ "type": "boolean" })
    );

    let graph = ws.response_schema(&ScopeRequest::for_policy("pricing"));
    assert_eq!(
        graph["properties"]["total"],
        json!({ "type": "number" }),
        "{graph}"
    );
    assert_eq!(graph["required"], json!(["total"]));
}

#[test]
fn unknown_paths_are_open_objects() {
    let ws = workspace();
    let open = json!({ "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" });
    assert_eq!(request(&ws, "missing", SchemaAudience::Contract), open);
    assert_eq!(
        ws.response_schema(&ScopeRequest::for_policy("missing")),
        open
    );
}

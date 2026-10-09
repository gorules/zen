//! Stored relationships: a `relationship` with `on` (members sharing this
//! entity's key) or `through` (counterparties in an events entity). The host
//! finds the members; features on the owner aggregate their features.

use serde_json::{json, Value};
use zen_engine::policy::{PolicyWorkspace, ScopeRequest, Severity, SuppliedBy};

fn prop(name: &str, ty: &str, extra: Value) -> Value {
    let mut p = json!({ "id": name, "name": name, "type": ty });
    if let (Some(p), Some(extra)) = (p.as_object_mut(), extra.as_object()) {
        p.extend(extra.clone());
    }
    p
}

fn entity(id: &str, data: Value) -> Value {
    json!({ "id": id, "type": "dataModel", "props": { "data": data } })
}

/// Merchants with a monthly sales feature, a peer group of merchants by
/// (mcc, postal_code), and transfers between customers.
fn doc(rollup: &str, merchant_extra: Vec<Value>, senders: Value, rule: &str) -> PolicyWorkspace {
    let mut merchant_props = vec![
        prop("id", "string", json!({})),
        prop("mcc", "string", json!({})),
        prop("postal_code", "string", json!({})),
        prop("risk", "string", json!({})),
        prop("sales", "number", json!({ "feature": { "expr": "sum(txn as t, t.amount)", "window": ["30d"] } })),
        prop("peer_group", "reference", json!({ "target": "peer_group" })),
    ];
    merchant_props.extend(merchant_extra);
    let blocks = json!([
        entity("b-txn", json!({
            "name": "txn", "events": { "id": "id", "time": "at" },
            "properties": [
                prop("id", "string", json!({})), prop("at", "date", json!({})), prop("amount", "number", json!({})),
                prop("merchant", "reference", json!({ "target": "merchant" })),
                prop("sender", "reference", json!({ "target": "customer" })),
                prop("receiver", "reference", json!({ "target": "customer" }))
            ]
        })),
        entity("b-merchant", json!({ "name": "merchant", "key": "id", "reference": {}, "properties": merchant_props })),
        entity("b-group", json!({
            "name": "peer_group", "key": ["mcc", "postal_code"], "reference": {},
            "properties": [
                prop("mcc", "string", json!({})), prop("postal_code", "string", json!({})),
                prop("merchants", "relationship", json!({ "target": "merchant", "array": true, "on": { "mcc": "mcc", "postal_code": "postal_code" } })),
                prop("avg_sales", "number", json!({ "feature": { "expr": rollup } }))
            ]
        })),
        entity("b-customer", json!({
            "name": "customer", "key": "id", "reference": {},
            "properties": [
                prop("id", "string", json!({})),
                prop("sent", "number", json!({ "feature": { "expr": "sum(txn as t, t.amount)", "window": ["30d"] } })),
                prop("senders", "relationship", json!({ "target": "customer", "array": true, "through": senders })),
                prop("big_senders", "number", json!({ "feature": { "expr": "count(senders as s, s.sent_30d > 1000 and s.id != id)" } }))
            ]
        })),
        { "id": "r", "type": "expression", "props": { "data": { "key": "txn.flag", "value": rule } } }
    ]);
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(json!({ "blocks": blocks })).unwrap());
    ws
}

fn errors(ws: &PolicyWorkspace) -> Vec<String> {
    ws.diagnostics("p")
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect()
}

fn through() -> Value {
    json!({ "events": "txn", "self": "receiver", "member": "sender", "window": "30d", "where": "amount >= 100" })
}

#[test]
fn rollups_aggregate_the_members_features() {
    let ws = doc("avg(merchants as m, m.sales_30d)", vec![], through(), "(txn.merchant.peer_group.avg_sales ?? 0) > 0");
    assert!(errors(&ws).is_empty(), "{:#?}", errors(&ws));

    // A member property that doesn't exist; a condition on the member.
    let ws = doc("avg(merchants as m, m.salez_30d)", vec![], through(), "true");
    assert!(errors(&ws).iter().any(|m| m.contains("salez_30d")), "{:#?}", errors(&ws));
    let ws = doc("count(merchants as m, m.risk == 'High')", vec![], through(), "true");
    assert!(errors(&ws).is_empty(), "{:#?}", errors(&ws));
}

#[test]
fn the_targets_stay_roots_and_the_members_are_the_hosts() {
    // Only the stored relationship points at merchant: it stays a root (the request is `{ merchant }`).
    let blocks = json!([
        entity("b-merchant", json!({ "name": "merchant", "key": "id", "reference": {}, "properties": [
            prop("id", "string", json!({})), prop("mcc", "string", json!({})), prop("postal_code", "string", json!({})),
            prop("peer_avg", "number", json!({ "feature": { "expr": "peer_group.avg_sales" } })),
            prop("peer_group", "reference", json!({ "target": "peer_group" }))
        ] })),
        entity("b-group", json!({ "name": "peer_group", "key": ["mcc", "postal_code"], "reference": {}, "properties": [
            prop("mcc", "string", json!({})), prop("postal_code", "string", json!({})),
            prop("merchants", "relationship", json!({ "target": "merchant", "array": true, "on": { "mcc": "mcc", "postal_code": "postal_code" } })),
            prop("avg_sales", "number", json!({ "feature": { "expr": "count(merchants as m, m.mcc != '')" } }))
        ] })),
        { "id": "r", "type": "expression", "props": { "data": { "key": "merchant.flag", "value": "merchant.mcc == '5411'" } } }
    ]);
    let mut ws = PolicyWorkspace::new();
    ws.set_policy("p", serde_json::from_value(json!({ "blocks": blocks })).unwrap());
    assert!(errors(&ws).is_empty(), "{:#?}", errors(&ws));
    let inputs = ws.inputs(&ScopeRequest::for_policy("p"));
    let path = |p: &str| inputs.iter().find(|i| i.path.as_ref() == p);
    let mcc = path("merchant.mcc").unwrap_or_else(|| panic!("merchant is a root: {inputs:#?}"));
    assert_eq!(mcc.supplied_by, SuppliedBy::Request);
    // The members never come with the request.
    assert!(
        inputs.iter().all(|i| !i.path.contains("merchants") || i.supplied_by == SuppliedBy::Host),
        "{inputs:#?}"
    );
}

#[test]
fn on_and_through_are_checked() {
    let ok = doc("avg(merchants as m, m.sales_30d)", vec![], through(), "true");
    assert!(errors(&ok).is_empty(), "{:#?}", errors(&ok));

    let found = errors(&doc(
        "avg(merchants as m, m.sales_30d)",
        vec![],
        json!({ "events": "merchant", "self": "receiver", "member": "nobody", "window": "a month" }),
        "true",
    ));
    for expected in ["`merchant` isn't an events entity", "a duration like 10m, 1h or 30d"] {
        assert!(found.iter().any(|m| m.contains(expected)), "{expected}: {found:#?}");
    }
    let found = errors(&doc(
        "avg(merchants as m, m.sales_30d)",
        vec![],
        json!({ "events": "txn", "self": "merchant", "member": "amount", "window": "30d" }),
        "true",
    ));
    for expected in ["`self` is `merchant`", "`member` is `amount`"] {
        assert!(found.iter().any(|m| m.contains(expected)), "{expected}: {found:#?}");
    }
}


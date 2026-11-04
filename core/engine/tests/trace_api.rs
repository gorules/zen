use serde_json::json;
use zen_engine::trace::{
    convert, restore, restore_json, ConvertArgs, Input, LegacyTrace, NodeError, RestoreArgs,
    Status, TraceError,
};
use zen_engine::Variable;

fn var(v: serde_json::Value) -> Variable {
    Variable::from(v)
}

fn graph_trace() -> serde_json::Value {
    json!({
        "input": {"id": "input", "input": null, "name": "input", "order": 0, "output": {"a": 1, "b": 2}, "performance": "500.0ns", "traceData": null},
        "calc": {"id": "calc", "input": {"a": 1, "b": 2}, "name": "calc", "order": 1, "output": {"total": 3}, "performance": "459.8µs", "traceData": {"total": {"result": 3}}},
        "output": {"id": "output", "input": {"total": 3}, "name": "output", "order": 2, "output": null, "performance": "667.0ns", "traceData": null}
    })
}

fn policy_documents() -> serde_json::Value {
    json!([{"blocks": [
        {"id": "model:customer", "type": "dataModel", "props": {"data": {"name": "customer", "scope": "entity", "properties": [{"array": false, "id": "customer.age", "name": "age", "optional": false, "type": "number"}]}}},
        {"id": "b0", "type": "expression", "props": {"data": {"key": "customer.adult", "value": "customer.age >= 18"}}}
    ], "imports": []}])
}

fn policy_trace() -> serde_json::Value {
    json!({"engineVersion": "2.1.2", "executions": [{"blockId": "b0", "operandValues": {"customer.age": 20}, "reads": ["customer.age"], "trace": {"kind": "expression", "property": "customer.adult", "value": true}, "writes": [{"path": "customer.adult", "value": true}]}], "properties": {"customer.adult": true}})
}

#[test]
fn graph_round_trip_from_variables() {
    let trace = var(graph_trace());
    let request = var(json!({"a": 1, "b": 2}));
    let mut args = ConvertArgs::new(LegacyTrace::Variable(&trace), Input::Variable(&request));
    args.engine_version = Some("2.1.2");
    let out = convert(args).unwrap();
    assert_eq!(out.status, Status::Converted);
    assert_eq!(
        out.json,
        r#"{"version":1,"kind":"graph","engine":"2.1.2","nodes":[{"id":"input","type":"input","perf":500},{"id":"calc","type":"expression","perf":459800,"results":{"total":3}},{"id":"output","type":"output","perf":667}]}"#
    );
    let doc = out.document();
    let back = restore(RestoreArgs {
        trace: Input::Variable(&doc),
        request: Input::Variable(&request),
        documents: None,
    })
    .unwrap();
    assert_eq!(back.to_value(), graph_trace());
}

#[test]
fn reference_string_traces_convert() {
    let trace = var(graph_trace());
    let reference = serde_json::to_string(&trace.serialize_ref()).unwrap();
    let request = var(json!({"a": 1, "b": 2}));
    let out = convert(ConvertArgs::new(
        LegacyTrace::ReferenceString(&reference),
        Input::Variable(&request),
    ))
    .unwrap();
    assert_eq!(out.status, Status::Converted);
    let back = restore_json(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Variable(&request),
        documents: None,
    })
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&back).unwrap(),
        graph_trace()
    );
}

#[test]
fn failed_node_carries_the_error() {
    let trace = json!({
        "input": {"id": "input", "input": null, "name": "input", "order": 0, "output": {"a": "x", "b": 1}, "performance": "292.0ns", "traceData": null},
        "calc": {"id": "calc", "input": {"a": "x", "b": 1}, "name": "calc", "order": 1, "output": null, "performance": "131.9µs", "traceData": {}}
    });
    let text = trace.to_string();
    let mut args = ConvertArgs::new(LegacyTrace::Json(&text), Input::Json(r#"{"a":"x","b":1}"#));
    args.error = Some(NodeError {
        node: "calc",
        message: Some("Failed to evaluate expression: \"a + b\""),
    });
    let out = convert(args).unwrap();
    assert!(
        out.json
            .contains(r#""failed":true,"error":"Failed to evaluate expression: \"a + b\"""#),
        "{}",
        out.json
    );
    let back = restore_json(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Json(r#"{"a":"x","b":1}"#),
        documents: None,
    })
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&back).unwrap(),
        trace
    );
}

#[test]
fn decimals_compare_exactly() {
    let text = r#"{"input":{"id":"input","input":null,"name":"input","order":0,"output":{"n":12345678901234567890.123456789},"performance":"250.0ns","traceData":null},"e":{"id":"e","input":{"n":12345678901234567890.123456789},"name":"e","order":1,"output":{"r":0.1000000000000000000000000001},"performance":"4.2µs","traceData":{"r":{"result":0.1000000000000000000000000001}}}}"#;
    let request = r#"{"n":12345678901234567890.123456789}"#;
    let out = convert(ConvertArgs::new(
        LegacyTrace::Json(text),
        Input::Json(request),
    ))
    .unwrap();
    assert_eq!(out.status, Status::Converted);
    assert!(out.json.contains("0.1000000000000000000000000001"));
    let back = restore_json(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Json(request),
        documents: None,
    })
    .unwrap();
    assert!(
        back.contains("12345678901234567890.123456789")
            && back.contains("0.1000000000000000000000000001"),
        "{back}"
    );
}

#[test]
fn policy_reads_come_from_the_documents() {
    let trace = var(policy_trace());
    let request = var(json!({"customer": {"age": 20}}));
    let docs = var(policy_documents());
    let mut args = ConvertArgs::new(LegacyTrace::Variable(&trace), Input::Variable(&request));
    args.documents = Some(Input::Variable(&docs));
    let out = convert(args).unwrap();
    assert_eq!(
        out.json,
        r#"{"version":1,"kind":"policy","engine":"2.1.2","requires":["documents"],"executions":[{"block":"b0","kind":"expression","writes":{"/customer/adult":true}}]}"#
    );
    let without = restore(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Variable(&request),
        documents: None,
    });
    assert!(matches!(without, Err(TraceError::DocumentsRequired)));
    let back = restore(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Variable(&request),
        documents: Some(Input::Variable(&docs)),
    })
    .unwrap();
    assert_eq!(back.to_value(), policy_trace());
}

#[test]
fn policy_without_documents_stores_reads() {
    let trace = var(policy_trace());
    let request = var(json!({"customer": {"age": 20}}));
    let out = convert(ConvertArgs::new(
        LegacyTrace::Variable(&trace),
        Input::Variable(&request),
    ))
    .unwrap();
    assert!(
        out.json.contains(r#""reads":["/customer/age"]"#),
        "{}",
        out.json
    );
    assert!(!out.json.contains("requires"));
    let back = restore(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Variable(&request),
        documents: None,
    })
    .unwrap();
    assert_eq!(back.to_value(), policy_trace());
}

#[test]
fn readers_refuse_what_they_do_not_know() {
    let request = Input::Json("{}");
    let refused = |doc: &str| {
        restore_json(RestoreArgs {
            trace: Input::Json(doc),
            request,
            documents: None,
        })
        .is_err()
    };
    assert!(refused(r#"{"version":2,"kind":"graph","nodes":[]}"#));
    assert!(refused(
        r#"{"version":1,"kind":"graph","view":"extended","nodes":[]}"#
    ));
    assert!(refused(
        r#"{"version":1,"kind":"graph","requires":["teleport"],"nodes":[]}"#
    ));
    assert!(refused(
        r#"{"version":1,"kind":"graph","nodes":[{"id":"a","perf":1,"output":{"$unknown":1}}]}"#
    ));
    assert!(refused(
        r#"{"version":1,"kind":"graph","requires":["derived"],"nodes":[]}"#
    ));
    assert!(!refused(
        r#"{"version":1,"kind":"graph","nodes":[{"id":"a","perf":1,"output":{"$literal":{"$unknown":1}},"note":"readers ignore unknown fields"}]}"#
    ));
}

#[test]
fn timings_print_like_the_engine() {
    for (ns, text) in [
        (333u64, "333.0ns"),
        (1050, "1.0µs"),
        (1150, "1.2µs"),
        (1450, "1.4µs"),
        (47200, "47.2µs"),
        (999_950, "1000.0µs"),
        (1_500_000_000, "1.5s"),
    ] {
        let doc = format!(
            r#"{{"version":1,"kind":"graph","nodes":[{{"id":"a","perf":{ns},"output":null}}]}}"#
        );
        let back = restore_json(RestoreArgs {
            trace: Input::Json(&doc),
            request: Input::Json("null"),
            documents: None,
        })
        .unwrap();
        assert!(
            back.contains(&format!(r#""performance":"{text}""#)),
            "{ns}: {back}"
        );
        assert_eq!(format!("{:.1?}", std::time::Duration::from_nanos(ns)), text);
    }
}

#[test]
fn unreadable_traces_are_stored_verbatim() {
    let text = r#"{"a":{"id":"b","input":null,"name":"a","order":0,"output":1,"performance":"1.0ns","traceData":null}}"#;
    let out = convert(ConvertArgs::new(
        LegacyTrace::Json(text),
        Input::Json("null"),
    ))
    .unwrap();
    assert_eq!(out.status, Status::Converted);
    assert!(out.json.contains(r#""verbatim""#));
    let back = restore_json(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Json("null"),
        documents: None,
    })
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&back).unwrap(),
        serde_json::from_str::<serde_json::Value>(text).unwrap()
    );
}

#[test]
fn reference_strings_keep_every_digit() {
    let reference = r#"{"$refs":["input","a"],"$root":{"@0":{"id":"@0","input":null,"name":"@0","order":0,"output":{"n":12345678901234567890.123,"s":"@1","e":"@@x"},"performance":"1.0µs","traceData":null}}}"#;
    let out = convert(ConvertArgs::new(
        LegacyTrace::ReferenceString(reference),
        Input::Json("{}"),
    ))
    .unwrap();
    assert_eq!(out.status, Status::Converted);
    assert!(
        out.json
            .contains(r#""output":{"n":12345678901234567890.123,"s":"a","e":"@x"}"#),
        "{}",
        out.json
    );
}

#[test]
fn loop_iterations_with_reordered_results_restore() {
    use zen_engine::trace::testing::{convert_val_with, first_diff, parse, restore_val};
    let text = include_str!("fixtures/trace/loop-key-order.json");
    let fx = parse(text).unwrap();
    let trace = fx.get("trace").unwrap();
    let request = fx.get("request").unwrap();
    let error = fx.get("error").unwrap();
    let node = error.get("node").and_then(|n| n.as_str()).unwrap();
    let message = error.get("message").and_then(|n| n.as_str());
    let (doc, status) = convert_val_with(trace, request, Some((node, message)), None, None, false);
    assert_eq!(status, Status::Converted);
    let back = restore_val(&doc, request, None).unwrap();
    assert_eq!(first_diff(trace, &back), None, "{doc}");
}

#[test]
fn policy_absence_is_a_document_default_with_record_overrides() {
    let lean = |id: &str| json!({"blockId": id, "trace": {"kind": "expression", "property": "customer.adult", "value": true}});
    let full = policy_trace()["executions"][0].clone();
    let trace = json!({"engineVersion": "2.1.2", "executions": [lean("b0"), full, lean("b1"), lean("b2")], "properties": {"customer.adult": true}});
    let request = var(json!({"customer": {"age": 20}}));
    let out = convert(ConvertArgs::new(
        LegacyTrace::Variable(&var(trace.clone())),
        Input::Variable(&request),
    ))
    .unwrap();
    assert!(
        out.json.contains(r#""requires":["absent-default"]"#),
        "{}",
        out.json
    );
    assert!(
        out.json
            .contains(r#""absent":["writes","reads","operands"]"#),
        "{}",
        out.json
    );
    assert_eq!(out.json.matches("Absent").count(), 3, "{}", out.json);
    assert!(out.json.contains(r#""writesAbsent":false"#), "{}", out.json);
    let back = restore(RestoreArgs {
        trace: Input::Json(&out.json),
        request: Input::Variable(&request),
        documents: None,
    })
    .unwrap();
    assert_eq!(back.to_value(), trace);
    let old = r#"{"version":1,"kind":"policy","executions":[{"block":"b0","kind":"expression","writesAbsent":true,"readsAbsent":true,"operandsAbsent":true,"details":{"property":"customer.adult","value":true}}],"properties":{"absent":true}}"#;
    let back = restore(RestoreArgs {
        trace: Input::Json(old),
        request: Input::Variable(&request),
        documents: None,
    })
    .unwrap();
    assert_eq!(back.to_value(), json!({"executions": [lean("b0")]}));
    let unknown = r#"{"version":1,"kind":"policy","requires":["absent-default"],"absent":["writes","sparks"],"executions":[]}"#;
    assert!(restore_json(RestoreArgs {
        trace: Input::Json(unknown),
        request: Input::Variable(&request),
        documents: None,
    })
    .is_err());
}

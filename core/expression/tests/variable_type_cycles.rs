use std::cell::RefCell;
use std::rc::Rc;

use zen_expression::variable::{VariableMapExt, VariableType};

type Fields = ahash::HashMap<Rc<str>, VariableType>;

fn cyclic_pair() -> (VariableType, VariableType) {
    let claim = VariableType::Object(Rc::new(RefCell::new(Fields::new())));
    let address = VariableType::Object(Rc::new(RefCell::new(Fields::new())));
    let VariableType::Object(claim_fields) = &claim else {
        unreachable!()
    };
    let VariableType::Object(address_fields) = &address else {
        unreachable!()
    };
    claim_fields
        .borrow_mut()
        .insert(Rc::from("amount"), VariableType::Number);
    claim_fields
        .borrow_mut()
        .insert(Rc::from("address"), address.shallow_clone());
    claim_fields
        .borrow_mut()
        .insert(Rc::from("items"), address.shallow_clone().array());
    address_fields
        .borrow_mut()
        .insert(Rc::from("city"), VariableType::String);
    address_fields
        .borrow_mut()
        .insert(Rc::from("claim"), claim.shallow_clone());
    (claim, address)
}

#[test]
fn serialize_cuts_cycles_and_keeps_the_derived_format() {
    let (claim, _) = cyclic_pair();
    let json = serde_json::to_value(&claim).expect("cyclic type serialises");
    assert_eq!(json["Object"]["amount"], "Number");
    assert_eq!(json["Object"]["address"]["Object"]["city"], "String");
    assert_eq!(
        json["Object"]["address"]["Object"]["claim"],
        serde_json::json!({ "Object": {} })
    );
    assert_eq!(
        json["Object"]["items"]["Array"]["Object"]["claim"],
        serde_json::json!({ "Object": {} })
    );
}

#[test]
fn serialize_round_trips_acyclic_types() {
    let mut fields = Fields::new();
    fields.insert(
        Rc::from("a"),
        VariableType::Nullable(Rc::new(VariableType::Enum(
            Some(Rc::from("status")),
            vec![Rc::from("open"), Rc::from("closed")],
        )))
        .array(),
    );
    fields.insert(Rc::from("c"), VariableType::Const(Rc::from("v")));
    fields.insert(Rc::from("d"), VariableType::Date);
    let original = VariableType::Object(Rc::new(RefCell::new(fields)));
    let json = serde_json::to_string(&original).expect("serialise");
    let back: VariableType = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, original);
}

#[test]
fn merge_and_satisfies_terminate_on_cyclic_objects() {
    let (claim, address) = cyclic_pair();
    let merged = claim.merge(&claim);
    assert!(matches!(merged, VariableType::Object(_)));
    assert!(claim.satisfies(&claim));
    assert!(claim.get("address").satisfies(&address));

    let (other_claim, _) = cyclic_pair();
    let merged = claim.merge(&other_claim);
    assert!(matches!(merged, VariableType::Object(_)));
    assert!(claim.satisfies(&other_claim));
    assert!(claim
        .merge(&VariableType::Number)
        .satisfies(&VariableType::Any));
}

fn branching_cycle(leaf: VariableType) -> VariableType {
    let nodes: Vec<_> = (0..3).map(|_| VariableType::empty_object()).collect();
    for (i, node) in nodes.iter().enumerate() {
        node.dot_insert("left", nodes[(i + 1) % 3].shallow_clone());
        node.dot_insert("right", nodes[(i + 2) % 3].shallow_clone());
        node.dot_insert("leaf", leaf.shallow_clone());
    }
    nodes[0].shallow_clone()
}

#[test]
fn separately_allocated_branching_cycles_are_bounded_and_check_leaves() {
    let a = branching_cycle(VariableType::Number);
    let b = branching_cycle(VariableType::Number);
    assert!(a.satisfies(&b));
    assert!(!a.satisfies(&branching_cycle(VariableType::Bool)));
    let merged = a.merge(&b);
    assert_eq!(merged.get("leaf"), VariableType::Number);
    assert_eq!(merged.get("left").get("leaf"), VariableType::Number);
    let mut nested = merged;
    for _ in 0..12 {
        nested = nested.get("left");
        assert_eq!(nested.get("leaf"), VariableType::Number);
    }
    assert!(format!("{a:?}").contains("Object(<recursive>)"));
}

#[test]
fn debug_preserves_existing_acyclic_format() {
    let value = VariableType::empty_object();
    value.dot_insert("age", VariableType::Number.array());
    assert_eq!(
        format!("{value:?}"),
        "Object(RefCell { value: {\"age\": Array(Number)} })"
    );
    assert_eq!(
        format!(
            "{:?}",
            VariableType::Nullable(Rc::new(VariableType::String))
        ),
        "Nullable(String)"
    );
}

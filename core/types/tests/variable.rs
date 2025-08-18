use rust_decimal_macros::dec;
use serde_json::json;
use std::error::Error;
use std::rc::Rc;
use zen_types::variable::Variable;

type TestResult = Result<(), Box<dyn Error>>;
#[test]
fn dot_operations() -> TestResult {
    let var = Variable::from(json!({
        "user": {
            "profile": {
                "name": "Alice",
                "age": 30
            }
        }
    }));

    // Test dot get
    assert_eq!(
        var.dot("user.profile.name"),
        Some(Variable::String(Rc::from("Alice")))
    );
    assert_eq!(var.dot("user.profile.nonexistent"), None);
    assert_eq!(var.dot("nonexistent.path"), None);

    // Test dot insert
    let updated = var.dot_insert(
        "user.profile.email",
        Variable::String(Rc::from("alice@example.com")),
    );
    assert!(updated.is_none()); // Returns previous value (none)
    assert_eq!(
        var.dot("user.profile.email"),
        Some(Variable::String(Rc::from("alice@example.com")))
    );

    // Test dot insert detached
    let new_var = var
        .dot_insert_detached("settings.theme", Variable::String(Rc::from("dark")))
        .ok_or_else(|| "Failed to insert detached path".to_string())?;
    assert_eq!(
        new_var.dot("settings.theme"),
        Some(Variable::String(Rc::from("dark")))
    );
    assert_eq!(var.dot("settings.theme"), None); // Original unchanged

    // Test dot remove
    let removed = var.dot_remove("user.profile.age");
    assert_eq!(removed, Some(Variable::Number(dec!(30))));
    assert_eq!(var.dot("user.profile.age"), None);

    Ok(())
}

#[test]
fn clone_operations() -> TestResult {
    let original = Variable::from(json!({
        "data": [1, 2, {"nested": "value"}],
        "count": 42
    }));

    // Test shallow clone - shares references
    let shallow = original.shallow_clone();
    if let (Variable::Array(orig_arr), Variable::Array(shallow_arr)) = (&original, &shallow) {
        assert!(Rc::ptr_eq(orig_arr, shallow_arr));
    }

    // Test depth clone
    let depth1 = original.depth_clone(1);
    if let (Variable::Array(orig_arr), Variable::Array(depth_arr)) = (&original, &depth1) {
        assert!(!Rc::ptr_eq(orig_arr, depth_arr)); // Different array refs

        let orig_nested = &orig_arr.borrow()[2];
        let depth_nested = &depth_arr.borrow()[2];
        if let (Variable::Object(orig_obj), Variable::Object(depth_obj)) =
            (orig_nested, depth_nested)
        {
            assert!(Rc::ptr_eq(orig_obj, depth_obj)); // Nested still shared at depth 1
        }
    }

    // Test deep clone - everything separate
    let deep = original.deep_clone();
    if let (Variable::Array(orig_arr), Variable::Array(deep_arr)) = (&original, &deep) {
        assert!(!Rc::ptr_eq(orig_arr, deep_arr));

        let orig_nested = &orig_arr.borrow()[2];
        let deep_nested = &deep_arr.borrow()[2];
        if let (Variable::Object(orig_obj), Variable::Object(deep_obj)) = (orig_nested, deep_nested)
        {
            assert!(!Rc::ptr_eq(orig_obj, deep_obj)); // Nested also separate
        }
    }

    Ok(())
}

#[test]
fn merge_operations() -> TestResult {
    let mut doc = Variable::from(json!({
        "user": {"name": "Alice", "age": 30},
        "settings": {"theme": "light"}
    }));

    let patch = Variable::from(json!({
        "user": {"age": 31, "email": "alice@example.com"},
        "settings": {"notifications": true},
        "new_field": "value"
    }));

    // Test merge clone (doesn't modify original)
    let merged = doc.merge_clone(&patch);
    assert_eq!(doc.dot("user.age"), Some(Variable::Number(dec!(30)))); // Original unchanged
    assert_eq!(merged.dot("user.age"), Some(Variable::Number(dec!(31)))); // Merged updated
    assert_eq!(
        merged.dot("user.email"),
        Some(Variable::String(Rc::from("alice@example.com")))
    );
    assert_eq!(
        merged.dot("new_field"),
        Some(Variable::String(Rc::from("value")))
    );

    // Test in-place merge
    doc.merge(&patch);
    assert_eq!(doc.dot("user.age"), Some(Variable::Number(dec!(31)))); // Original now changed
    assert_eq!(
        doc.dot("user.name"),
        Some(Variable::String(Rc::from("Alice")))
    ); // Preserved
    assert_eq!(
        doc.dot("settings.notifications"),
        Some(Variable::Bool(true))
    ); // Added

    // Test null removal
    let null_patch = Variable::from(json!({"user": {"name": null}}));
    doc.merge(&null_patch);
    assert_eq!(doc.dot("user.name"), None); // Removed by null

    Ok(())
}

#[test]
fn type_operations() -> TestResult {
    let var = Variable::from(json!({
        "string": "hello",
        "number": 42,
        "bool": true,
        "array": [1, 2, 3],
        "object": {"key": "value"},
        "null": null
    }));

    // Test type checks
    assert!(var.dot("array").unwrap().is_array());
    assert!(var.dot("object").unwrap().is_object());
    assert!(!var.dot("string").unwrap().is_array());

    // Test accessors
    assert_eq!(var.dot("string").unwrap().as_str(), Some("hello"));
    assert_eq!(var.dot("number").unwrap().as_number(), Some(dec!(42)));
    assert_eq!(var.dot("bool").unwrap().as_bool(), Some(true));
    assert!(var.dot("array").unwrap().as_array().is_some());
    assert!(var.dot("object").unwrap().as_object().is_some());

    // Test type names
    assert_eq!(var.dot("string").unwrap().type_name(), "string");
    assert_eq!(var.dot("number").unwrap().type_name(), "number");
    assert_eq!(var.dot("bool").unwrap().type_name(), "bool");
    assert_eq!(var.dot("array").unwrap().type_name(), "array");
    assert_eq!(var.dot("object").unwrap().type_name(), "object");
    assert_eq!(var.dot("null").unwrap().type_name(), "null");

    Ok(())
}

#[test]
fn edge_cases() -> TestResult {
    // Empty structures
    let empty_obj = Variable::empty_object();
    let empty_arr = Variable::empty_array();
    assert!(empty_obj.is_object());
    assert!(empty_arr.is_array());

    // Dot operations on non-objects
    let number = Variable::Number(dec!(42));
    assert_eq!(number.dot("anything"), None);
    assert_eq!(number.dot_insert("path", Variable::Null), None);
    assert_eq!(number.dot_remove("path"), None);

    // Array merge at top level
    let mut doc = Variable::from(json!({"key": "value"}));
    let array_patch = Variable::from(json!([1, 2, 3]));
    doc.merge(&array_patch);
    assert!(doc.is_array()); // Replaced with array

    // Self-merge (no-op)
    let mut original = Variable::from(json!({"a": 1}));
    let clone = original.shallow_clone();
    original.merge(&clone);
    assert_eq!(original.dot("a"), Some(Variable::Number(dec!(1))));

    Ok(())
}

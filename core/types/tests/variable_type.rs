use ahash::{HashMap, HashMapExt};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use zen_types::variable_type::VariableType;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn variable_type_operations() -> TestResult {
    // Test basic types and accessors
    assert!(VariableType::Array(Rc::new(VariableType::Number)).is_array());
    assert!(VariableType::Interval.is_iterable());
    assert!(VariableType::String.is_string());
    assert!(VariableType::empty_object().is_object());
    assert!(VariableType::Null.is_null());

    // Test iterator
    assert_eq!(
        VariableType::Array(Rc::new(VariableType::String)).iterator(),
        Some(Rc::new(VariableType::String))
    );
    assert_eq!(
        VariableType::Interval.iterator(),
        Some(Rc::new(VariableType::Number))
    );
    assert_eq!(VariableType::String.iterator(), None);

    // Test const string extraction
    let const_type = VariableType::Const(Rc::from("test"));
    assert_eq!(const_type.as_const_str(), Some(Rc::from("test")));
    assert_eq!(VariableType::String.as_const_str(), None);

    // Test widen
    assert_eq!(const_type.widen(), VariableType::String);
    assert_eq!(
        VariableType::Enum(None, vec![Rc::from("a")]).widen(),
        VariableType::String
    );
    assert_eq!(VariableType::Number.widen(), VariableType::Number);

    Ok(())
}

#[test]
fn variable_type_satisfies() -> TestResult {
    // Basic type satisfaction
    assert!(VariableType::Any.satisfies(&VariableType::String));
    assert!(VariableType::String.satisfies(&VariableType::Any));
    assert!(VariableType::Number.satisfies(&VariableType::Number));
    assert!(!VariableType::String.satisfies(&VariableType::Number));

    // Date satisfaction
    assert!(VariableType::Number.satisfies(&VariableType::Date));
    assert!(VariableType::String.satisfies(&VariableType::Date));

    // Const and enum satisfaction
    let const_a = VariableType::Const(Rc::from("a"));
    let enum_ab = VariableType::Enum(None, vec![Rc::from("a"), Rc::from("b")]);
    assert!(const_a.satisfies(&VariableType::String));
    assert!(const_a.satisfies(&enum_ab));
    assert!(enum_ab.satisfies(&VariableType::String));

    // Array satisfaction
    let arr_num = VariableType::Array(Rc::new(VariableType::Number));
    let arr_str = VariableType::Array(Rc::new(VariableType::String));
    assert!(arr_num.satisfies(&arr_num));
    assert!(!arr_num.satisfies(&arr_str));

    Ok(())
}

#[test]
fn variable_type_merge() -> TestResult {
    // Basic merges
    assert_eq!(
        VariableType::Number.merge(&VariableType::String),
        VariableType::Any
    );
    assert_eq!(
        VariableType::String.merge(&VariableType::String),
        VariableType::String
    );
    assert_eq!(
        VariableType::Date.merge(&VariableType::Date),
        VariableType::Date
    );
    assert_eq!(
        VariableType::Interval.merge(&VariableType::Interval),
        VariableType::Interval
    );

    // Const merges
    let const_a = VariableType::Const(Rc::from("a"));
    let const_b = VariableType::Const(Rc::from("b"));
    let merged = const_a.merge(&const_b);
    assert!(matches!(merged, VariableType::Enum(None, _)));

    // Same const merge
    assert_eq!(const_a.merge(&const_a), const_a);

    // Const with string
    assert_eq!(const_a.merge(&VariableType::String), VariableType::String);

    // Const with enum and enum with const
    let enum_bc = VariableType::Enum(None, vec![Rc::from("b"), Rc::from("c")]);
    let const_enum_merge = const_a.merge(&enum_bc);
    if let VariableType::Enum(_, vals) = const_enum_merge {
        assert_eq!(vals.len(), 3);
    }

    let enum_const_merge = enum_bc.merge(&const_a);
    if let VariableType::Enum(_, vals) = enum_const_merge {
        assert_eq!(vals.len(), 3);
    }

    // Array merge with same pointer
    let shared_array_type = Rc::new(VariableType::Number);
    let arr1 = VariableType::Array(shared_array_type.clone());
    let arr2 = VariableType::Array(shared_array_type);
    assert_eq!(arr1.merge(&arr2), arr1);

    // Object merge
    let obj1 = VariableType::Object(Rc::new(RefCell::new({
        let mut map = HashMap::new();
        map.insert(Rc::from("key1"), VariableType::String);
        map.insert(Rc::from("shared"), VariableType::Number);
        map
    })));
    let obj2 = VariableType::Object(Rc::new(RefCell::new({
        let mut map = HashMap::new();
        map.insert(Rc::from("key2"), VariableType::Bool);
        map.insert(Rc::from("shared"), VariableType::String);
        map
    })));
    let merged_obj = obj1.merge(&obj2);
    if let VariableType::Object(obj_ref) = merged_obj {
        let obj_map = obj_ref.borrow();
        assert_eq!(obj_map.len(), 3);
        assert_eq!(obj_map.get("shared"), Some(&VariableType::Any));
    }

    // Enum merges
    let enum1 = VariableType::Enum(Some(Rc::from("E1")), vec![Rc::from("x")]);
    let enum2 = VariableType::Enum(Some(Rc::from("E2")), vec![Rc::from("y")]);
    let merged_enum = enum1.merge(&enum2);
    if let VariableType::Enum(name, vals) = merged_enum {
        assert!(name.unwrap().contains("E1 | E2"));
        assert_eq!(vals.len(), 2);
    }

    Ok(())
}

#[test]
fn variable_type_dot_operations() -> TestResult {
    let mut obj_map = HashMap::new();
    obj_map.insert(
        Rc::from("user"),
        VariableType::Object(Rc::new(RefCell::new({
            let mut user_map = HashMap::new();
            user_map.insert(Rc::from("name"), VariableType::String);
            user_map
        }))),
    );
    let obj = VariableType::Object(Rc::new(RefCell::new(obj_map)));

    // Test dot get
    assert_eq!(obj.dot("user.name"), Some(VariableType::String));
    assert_eq!(obj.dot("user.nonexistent"), None);
    assert_eq!(obj.dot("nonexistent"), None);

    // Test dot insert
    let prev = obj.dot_insert("user.email", VariableType::String);
    assert_eq!(prev, None); // No previous value
    assert_eq!(obj.dot("user.email"), Some(VariableType::String));

    // Test dot insert detached
    let new_obj = obj
        .dot_insert_detached("settings.theme", VariableType::String)
        .expect("should insert successfully");
    assert_eq!(new_obj.dot("settings.theme"), Some(VariableType::String));
    assert_eq!(obj.dot("settings.theme"), None);

    // Test invalid dot operations on non-objects
    assert_eq!(VariableType::String.dot("anything"), None);
    assert_eq!(
        VariableType::String.dot_insert("path", VariableType::Number),
        None
    );

    Ok(())
}

#[test]
fn variable_type_conversions() -> TestResult {
    // Test from serde_json Value
    assert_eq!(VariableType::from(json!(null)), VariableType::Null);
    assert_eq!(VariableType::from(json!(true)), VariableType::Bool);
    assert_eq!(VariableType::from(json!(42)), VariableType::Number);
    assert_eq!(VariableType::from(json!("hello")), VariableType::String);

    // Test array conversion with mixed types
    let mixed_array = json!([1, "hello", true]);
    let array_type = VariableType::from(mixed_array);
    assert!(matches!(array_type, VariableType::Array(_)));

    // Test empty array
    let empty_array = Vec::<serde_json::Value>::new();
    let empty_type = VariableType::from(empty_array);
    assert_eq!(empty_type, VariableType::Array(Rc::new(VariableType::Any)));

    // Test array reference conversion
    let vec_ref = vec![json!(1), json!("test")];
    let ref_type = VariableType::from(&vec_ref);
    assert!(matches!(ref_type, VariableType::Array(_)));

    let empty_vec_ref = Vec::<serde_json::Value>::new();
    let empty_ref_type = VariableType::from(&empty_vec_ref);
    assert_eq!(
        empty_ref_type,
        VariableType::Array(Rc::new(VariableType::Any))
    );

    // Test object conversion
    let obj = json!({"name": "Alice", "age": 30});
    let obj_type = VariableType::from(obj);
    assert!(matches!(obj_type, VariableType::Object(_)));
    if let VariableType::Object(obj_ref) = obj_type {
        let obj_map = obj_ref.borrow();
        assert!(obj_map.contains_key(&Rc::from("name")));
        assert!(obj_map.contains_key(&Rc::from("age")));
    }

    // Test convenience methods
    assert_eq!(
        VariableType::Number.array(),
        VariableType::Array(Rc::new(VariableType::Number))
    );
    assert_eq!(VariableType::default(), VariableType::Null);

    Ok(())
}

#[test]
fn variable_type_clone_operations() -> TestResult {
    let mut inner_map = HashMap::new();
    inner_map.insert(Rc::from("value"), VariableType::String);
    let inner_obj = VariableType::Object(Rc::new(RefCell::new(inner_map)));

    let mut outer_map = HashMap::new();
    outer_map.insert(Rc::from("inner"), inner_obj);
    let outer_obj = VariableType::Object(Rc::new(RefCell::new(outer_map)));

    // Test shallow clone - shares references
    let shallow = outer_obj.shallow_clone();
    if let (VariableType::Object(orig), VariableType::Object(clone)) = (&outer_obj, &shallow) {
        assert!(Rc::ptr_eq(orig, clone));
    }

    // Test depth clone
    let depth1 = outer_obj.depth_clone(1);
    if let (VariableType::Object(orig), VariableType::Object(clone)) = (&outer_obj, &depth1) {
        assert!(!Rc::ptr_eq(orig, clone)); // Outer different

        let orig = orig.borrow();
        let clone = clone.borrow();
        let orig_inner = orig.get("inner").unwrap();
        let clone_inner = clone.get("inner").unwrap();
        if let (VariableType::Object(orig_inner_ref), VariableType::Object(clone_inner_ref)) =
            (orig_inner, clone_inner)
        {
            assert!(Rc::ptr_eq(orig_inner_ref, clone_inner_ref)); // Inner still shared at depth 1
        }
    }

    Ok(())
}

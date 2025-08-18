use ahash::{HashMap, HashMapExt};
use rust_decimal_macros::dec;
use serde_json::json;
use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;
use zen_types::rcvalue::RcValue;
use zen_types::variable::Variable;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn serialize_deserialize_simple() -> TestResult {
    let var = Variable::from(json!({
        "name": "Alice",
        "age": 30,
        "active": true
    }));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_deserialize_with_refs() -> TestResult {
    let shared_string = "shared_value";
    let var = Variable::from(json!({
        "user1": {
            "name": shared_string,
            "status": shared_string
        },
        "user2": {
            "name": shared_string,
            "friend": shared_string
        },
        "metadata": {
            "type": shared_string
        }
    }));

    let serialized = var.serialize_ref();

    // Check that refs were created
    if let RcValue::Object(ref obj) = serialized {
        assert!(obj.contains_key(&Rc::from("$refs")));
        assert!(obj.contains_key(&Rc::from("$root")));
    } else {
        panic!("Expected object");
    }

    let deserialized = Variable::deserialize_ref(serialized)?;
    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_deserialize_array_refs() -> TestResult {
    let shared_array = vec![1, 2, 3];
    let var = Variable::from(json!({
        "data1": shared_array,
        "data2": shared_array,
        "backup": shared_array
    }));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_deserialize_at_string_escaping() -> TestResult {
    let var = Variable::from(json!({
        "normal": "hello",
        "at_string": "@special",
        "double_at": "@@escaped"
    }));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_deserialize_nested_structure() -> TestResult {
    let var = Variable::from(json!({
        "level1": {
            "level2": {
                "level3": {
                    "data": "deep_value",
                    "numbers": [1, 2, 3, 4, 5]
                }
            },
            "shared": "common_string"
        },
        "other": {
            "ref": "common_string"
        },
        "array": [
            {"shared": "common_string"},
            {"different": "unique"}
        ]
    }));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn no_refs_when_below_threshold() -> TestResult {
    // String too short, should not create refs
    let var = Variable::from(json!({
        "a": "hi",
        "b": "hi",
        "c": "hi"
    }));

    let serialized = var.serialize_ref();

    // Should not have refs section
    if let RcValue::Object(ref obj) = serialized {
        assert!(!obj.contains_key(&Rc::from("$refs")));
    }

    let deserialized = Variable::deserialize_ref(serialized)?;
    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_circular_references() -> TestResult {
    // Create a shared object that will be referenced multiple times
    let shared_obj = Rc::new(RefCell::new({
        let mut map = HashMap::new();
        map.insert(
            Rc::from("shared_data"),
            Variable::String(Rc::from("important_value")),
        );
        map.insert(Rc::from("id"), Variable::Number(dec!(42.0)));
        map
    }));

    // Create a structure where the same object appears in multiple places
    let mut root_map = HashMap::new();
    root_map.insert(Rc::from("first_ref"), Variable::Object(shared_obj.clone()));
    root_map.insert(Rc::from("second_ref"), Variable::Object(shared_obj.clone()));
    root_map.insert(Rc::from("third_ref"), Variable::Object(shared_obj));

    let var = Variable::Object(Rc::new(RefCell::new(root_map)));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_same_array_multiple_locations() -> TestResult {
    use std::cell::RefCell;

    // Create a shared array
    let shared_array = Rc::new(RefCell::new(vec![
        Variable::String(Rc::from("item_one")),
        Variable::String(Rc::from("item_two")),
        Variable::Number(dec!(123.0)),
    ]));

    let var = Variable::from(json!({
        "list1": shared_array.clone(),
        "backup_list": shared_array.clone(),
        "nested": {
            "inner_list": shared_array
        }
    }));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_mixed_shared_references() -> TestResult {
    let shared_string: Rc<str> = Rc::from("shared_between_key_and_value");

    // Create an object where the same string is used as both key and value
    let mut obj_map = HashMap::new();
    obj_map.insert(
        shared_string.clone(),
        Variable::String(shared_string.clone()),
    );
    obj_map.insert(
        Rc::from("other_key"),
        Variable::String(shared_string.clone()),
    );

    let shared_obj = Rc::new(RefCell::new(obj_map));

    // Use the shared object in multiple places
    let var = Variable::from(json!({
        "container1": shared_obj.clone(),
        "container2": shared_obj.clone(),
        "metadata": {
            "reference": shared_obj
        }
    }));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

#[test]
fn serialize_shared_array_with_shared_strings() -> TestResult {
    let shared_string: Rc<str> = Rc::from("shared_string_value");

    // Create a shared array containing the shared string
    let shared_array = Rc::new(RefCell::new(vec![
        Variable::String(shared_string.clone()),
        Variable::Number(dec!(42.0)),
        Variable::String(shared_string.clone()),
    ]));

    // Use the shared array in multiple places
    let mut root_map = HashMap::new();
    root_map.insert(Rc::from("array1"), Variable::Array(shared_array.clone()));
    root_map.insert(Rc::from("array2"), Variable::Array(shared_array.clone()));
    root_map.insert(Rc::from("array3"), Variable::Array(shared_array));

    let var = Variable::Object(Rc::new(RefCell::new(root_map)));

    let serialized = var.serialize_ref();
    let deserialized = Variable::deserialize_ref(serialized)?;

    assert_eq!(var, deserialized);

    Ok(())
}

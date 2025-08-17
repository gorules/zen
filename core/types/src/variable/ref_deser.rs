use crate::rcvalue::RcValue;
use crate::variable::Variable;
use ahash::{HashMap, HashMapExt};
use std::cell::RefCell;
use std::rc::Rc;

pub struct RefDeserializer {
    refs: Vec<Option<Variable>>,
}

impl RefDeserializer {
    pub fn new() -> Self {
        Self { refs: Vec::new() }
    }

    pub fn deserialize(&mut self, value: RcValue) -> Result<Variable, DeserializeError> {
        let RcValue::Object(mut root_obj) = value else {
            return Err(DeserializeError::InvalidFormat(
                "Expected root object".into(),
            ));
        };

        if let Some(RcValue::Array(refs_array)) = root_obj.remove(&Rc::from("$refs")) {
            self.refs = vec![None; refs_array.len()];

            for (i, _) in refs_array.iter().enumerate() {
                match &refs_array[i] {
                    RcValue::Array(_) => {
                        self.refs[i] = Some(Variable::Array(Rc::new(RefCell::new(Vec::new()))));
                    }
                    RcValue::Object(_) => {
                        self.refs[i] =
                            Some(Variable::Object(Rc::new(RefCell::new(HashMap::default()))));
                    }
                    _ => {
                        self.refs[i] = Some(self.deserialize_value(&refs_array[i])?);
                    }
                }
            }

            for (i, ref_value) in refs_array.iter().enumerate() {
                match ref_value {
                    RcValue::Array(arr) => {
                        if let Some(Variable::Array(target)) = &self.refs[i] {
                            let mut items = Vec::with_capacity(arr.len());
                            for item in arr {
                                items.push(self.deserialize_value(item)?);
                            }
                            *target.borrow_mut() = items;
                        }
                    }
                    RcValue::Object(obj) => {
                        if let Some(Variable::Object(target)) = &self.refs[i] {
                            let mut map = HashMap::with_capacity(obj.len());
                            for (key, value) in obj {
                                let key_var = self.deserialize_key(key)?;
                                let value_var = self.deserialize_value(value)?;
                                map.insert(key_var, value_var);
                            }
                            *target.borrow_mut() = map;
                        }
                    }
                    _ => {}
                }
            }
        }

        let root_value = root_obj
            .remove(&Rc::from("$root"))
            .ok_or_else(|| DeserializeError::InvalidFormat("Missing $root".into()))?;

        self.deserialize_value(&root_value)
    }

    fn deserialize_key(&self, key: &Rc<str>) -> Result<Rc<str>, DeserializeError> {
        if let Some(ref_id) = parse_ref_id(key) {
            if ref_id >= self.refs.len() {
                return Err(DeserializeError::InvalidReference(ref_id));
            }

            match &self.refs[ref_id] {
                Some(Variable::String(s)) => Ok(s.clone()),
                Some(_) => Err(DeserializeError::InvalidFormat(
                    "Reference used as key must be a string".into(),
                )),
                None => Err(DeserializeError::UnresolvedReference(ref_id)),
            }
        } else {
            Ok(unescape_at_string(key))
        }
    }

    fn deserialize_value(&self, value: &RcValue) -> Result<Variable, DeserializeError> {
        match value {
            RcValue::Null => Ok(Variable::Null),
            RcValue::Bool(b) => Ok(Variable::Bool(*b)),
            RcValue::Number(n) => Ok(Variable::Number(*n)),
            RcValue::String(s) => {
                if let Some(ref_id) = parse_ref_id(s) {
                    if ref_id >= self.refs.len() {
                        return Err(DeserializeError::InvalidReference(ref_id));
                    }

                    self.refs[ref_id]
                        .clone()
                        .ok_or(DeserializeError::UnresolvedReference(ref_id))
                } else {
                    Ok(Variable::String(unescape_at_string(s)))
                }
            }
            RcValue::Array(arr) => {
                let mut items = Vec::with_capacity(arr.len());
                for item in arr {
                    items.push(self.deserialize_value(item)?);
                }
                Ok(Variable::Array(Rc::new(RefCell::new(items))))
            }
            RcValue::Object(obj) => {
                let mut map = HashMap::with_capacity(obj.len());
                for (key, value) in obj {
                    let key_var = self.deserialize_key(key)?;
                    let value_var = self.deserialize_value(value)?;
                    map.insert(key_var, value_var);
                }
                Ok(Variable::Object(Rc::new(RefCell::new(map))))
            }
        }
    }
}

impl Default for RefDeserializer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum DeserializeError {
    InvalidFormat(String),
    InvalidReference(usize),
    UnresolvedReference(usize),
}

impl std::fmt::Display for DeserializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeserializeError::InvalidFormat(msg) => write!(f, "Invalid format: {}", msg),
            DeserializeError::InvalidReference(id) => write!(f, "Invalid reference: @{}", id),
            DeserializeError::UnresolvedReference(id) => write!(f, "Unresolved reference: @{}", id),
        }
    }
}

impl std::error::Error for DeserializeError {}

fn unescape_at_string(s: &Rc<str>) -> Rc<str> {
    if s.starts_with("@@") {
        Rc::from(&s[1..])
    } else {
        s.clone()
    }
}

fn parse_ref_id(s: &str) -> Option<usize> {
    s.strip_prefix('@')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::variable::ref_ser::RefSerializer;
    use serde_json::json;

    #[test]
    fn test_serialize_deserialize_simple() {
        let var = Variable::from(json!({
            "name": "Alice",
            "age": 30,
            "active": true
        }));

        let serializer = RefSerializer::new();
        let serialized = serializer.serialize(&var).unwrap();

        let mut deserializer = RefDeserializer::new();
        let deserialized = deserializer.deserialize(serialized).unwrap();

        // Should match the original
        assert_eq!(var, deserialized);
    }

    #[test]
    fn test_serialize_deserialize_with_refs() {
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

        let serializer = RefSerializer::new();
        let serialized = serializer.serialize(&var).unwrap();

        // Check that refs were created
        if let RcValue::Object(ref obj) = serialized {
            assert!(obj.contains_key(&Rc::from("$refs")));
            assert!(obj.contains_key(&Rc::from("$root")));
        } else {
            panic!("Expected object");
        }

        let mut deserializer = RefDeserializer::new();
        let deserialized = deserializer.deserialize(serialized).unwrap();

        assert_eq!(var, deserialized);
    }

    #[test]
    fn test_serialize_deserialize_array_refs() {
        let shared_array = vec![1, 2, 3];
        let var = Variable::from(json!({
            "data1": shared_array,
            "data2": shared_array,
            "backup": shared_array
        }));

        let serializer = RefSerializer::new();
        let serialized = serializer.serialize(&var).unwrap();

        let mut deserializer = RefDeserializer::new();
        let deserialized = deserializer.deserialize(serialized).unwrap();

        assert_eq!(var, deserialized);
    }

    #[test]
    fn test_serialize_deserialize_at_string_escaping() {
        let var = Variable::from(json!({
            "normal": "hello",
            "at_string": "@special",
            "double_at": "@@escaped"
        }));

        let serializer = RefSerializer::new();
        let serialized = serializer.serialize(&var).unwrap();

        let mut deserializer = RefDeserializer::new();
        let deserialized = deserializer.deserialize(serialized).unwrap();

        assert_eq!(var, deserialized);
    }

    #[test]
    fn test_serialize_deserialize_nested_structure() {
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

        let serializer = RefSerializer::new();
        let serialized = serializer.serialize(&var).unwrap();

        let mut deserializer = RefDeserializer::new();
        let deserialized = deserializer.deserialize(serialized).unwrap();

        assert_eq!(var, deserialized);
    }

    #[test]
    fn test_no_refs_when_below_threshold() {
        // String too short, should not create refs
        let var = Variable::from(json!({
            "a": "hi",
            "b": "hi",
            "c": "hi"
        }));

        let serializer = RefSerializer::new();
        let serialized = serializer.serialize(&var).unwrap();

        // Should not have refs section
        if let RcValue::Object(ref obj) = serialized {
            assert!(!obj.contains_key(&Rc::from("$refs")));
        }

        let mut deserializer = RefDeserializer::new();
        let deserialized = deserializer.deserialize(serialized).unwrap();

        assert_eq!(var, deserialized);
    }
}

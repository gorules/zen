use crate::rcvalue::RcValue;
use crate::variable::Variable;
use ahash::{HashMap, HashMapExt};
use std::cell::RefCell;
use std::rc::Rc;
use thiserror::Error;

pub struct RefDeserializer {
    refs: Vec<Option<Variable>>,
}

impl RefDeserializer {
    pub fn new() -> Self {
        Self { refs: Vec::new() }
    }

    pub fn deserialize(&mut self, value: RcValue) -> Result<Variable, RefDeserializeError> {
        let RcValue::Object(mut root_obj) = value else {
            return Err(RefDeserializeError::InvalidFormat(
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
            .ok_or_else(|| RefDeserializeError::InvalidFormat("Missing $root".into()))?;

        self.deserialize_value(&root_value)
    }

    fn deserialize_key(&self, key: &Rc<str>) -> Result<Rc<str>, RefDeserializeError> {
        if let Some(ref_id) = parse_ref_id(key) {
            if ref_id >= self.refs.len() {
                return Err(RefDeserializeError::InvalidReference(ref_id));
            }

            match &self.refs[ref_id] {
                Some(Variable::String(s)) => Ok(s.clone()),
                Some(_) => Err(RefDeserializeError::InvalidFormat(
                    "Reference used as key must be a string".into(),
                )),
                None => Err(RefDeserializeError::UnresolvedReference(ref_id)),
            }
        } else {
            Ok(unescape_at_string(key))
        }
    }

    fn deserialize_value(&self, value: &RcValue) -> Result<Variable, RefDeserializeError> {
        match value {
            RcValue::Null => Ok(Variable::Null),
            RcValue::Bool(b) => Ok(Variable::Bool(*b)),
            RcValue::Number(n) => Ok(Variable::Number(*n)),
            RcValue::String(s) => {
                if let Some(ref_id) = parse_ref_id(s) {
                    if ref_id >= self.refs.len() {
                        return Err(RefDeserializeError::InvalidReference(ref_id));
                    }

                    self.refs[ref_id]
                        .clone()
                        .ok_or(RefDeserializeError::UnresolvedReference(ref_id))
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

#[derive(Debug, Error)]
pub enum RefDeserializeError {
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
    #[error("Invalid reference: {0}")]
    InvalidReference(usize),
    #[error("UnresolvedReference: {0}")]
    UnresolvedReference(usize),
}

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

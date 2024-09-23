use crate::variable::types::VariableType;
use serde_json::Value;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::rc::Rc;

impl VariableType {
    pub fn array_item(&self) -> Option<Rc<VariableType>> {
        match self {
            VariableType::Array(item) => Some(item.clone()),
            _ => None,
        }
    }

    pub fn as_const_str(&self) -> Option<&str> {
        match self {
            VariableType::Constant(c) => match c.as_ref() {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn omit_const(&self) -> VariableType {
        match self {
            VariableType::Constant(v) => VariableType::from(v.as_ref()),
            _ => self.clone(),
        }
    }

    pub fn get(&self, vt: &VariableType) -> Rc<VariableType> {
        match self {
            VariableType::Array(inner) => inner.clone(),
            VariableType::Object(obj) => match vt.as_const_str() {
                None => Rc::new(VariableType::Any),
                Some(key) => obj.get(key).cloned().unwrap_or(Rc::new(VariableType::Any)),
            },
            VariableType::Any => Rc::new(VariableType::Any),
            VariableType::Constant(c) => match c.as_ref() {
                Value::Array(arr) => {
                    let arr_type = VariableType::from(arr.clone());
                    arr_type.array_item().unwrap_or(Rc::new(VariableType::Any))
                }
                Value::Object(obj) => match vt.as_const_str() {
                    None => Rc::new(VariableType::Any),
                    Some(key) => obj
                        .get(key)
                        .map(|v| Rc::new(v.into()))
                        .unwrap_or(Rc::new(VariableType::Any)),
                },
                _ => Rc::from(VariableType::Null),
            },
            _ => Rc::from(VariableType::Null),
        }
    }

    pub fn satisfies(&self, constraint: &Self) -> bool {
        match (self, constraint) {
            (VariableType::Any, _) | (_, VariableType::Any) => true,
            (VariableType::Null, VariableType::Null) => true,
            (VariableType::Bool, VariableType::Bool) => true,
            (VariableType::String, VariableType::String) => true,
            (VariableType::Number, VariableType::Number) => true,
            (VariableType::Array(a1), VariableType::Array(a2)) => a1 == a2,
            (VariableType::Object(o1), VariableType::Object(o2)) => o1
                .iter()
                .all(|(k, v)| o2.get(k).is_some_and(|tv| v.satisfies(tv))),
            (VariableType::Constant(c1), VariableType::Constant(c2)) => c1 == c2,
            (VariableType::Constant(c), _) => {
                let self_kind: VariableType = c.as_ref().into();
                self_kind.satisfies(constraint)
            }
            (_, _) => false,
        }
    }

    pub fn satisfies_array(&self) -> bool {
        match self {
            VariableType::Any | VariableType::Array(_) => true,
            VariableType::Constant(c) => match c.as_ref() {
                Value::Array(_) => true,
                _ => false,
            },
            _ => false,
        }
    }

    pub fn satisfies_object(&self) -> bool {
        match self {
            VariableType::Any | VariableType::Object(_) => true,
            VariableType::Constant(c) => match c.as_ref() {
                Value::Object(_) => true,
                _ => false,
            },
            _ => false,
        }
    }

    pub fn merge(&self, other: &Self) -> Self {
        match (&self, other) {
            (VariableType::Any, _) | (_, VariableType::Any) => VariableType::Any,
            (VariableType::Null, VariableType::Null) => VariableType::Null,
            (VariableType::Bool, VariableType::Bool) => VariableType::Bool,
            (VariableType::String, VariableType::String) => VariableType::String,
            (VariableType::Number, VariableType::Number) => VariableType::Number,
            (VariableType::Array(a1), VariableType::Array(a2)) => {
                if Rc::ptr_eq(&a1, &a2) {
                    VariableType::Array(a1.clone())
                } else {
                    VariableType::Array(Rc::new(a1.merge(a2)))
                }
            }
            (VariableType::Constant(c1), VariableType::Constant(c2)) => {
                if Rc::ptr_eq(&c1, &c2) {
                    VariableType::Constant(c1.clone())
                } else if c1 == c2 {
                    VariableType::Constant(c1.clone())
                } else {
                    let vt1 = VariableType::from(c1.as_ref());
                    let vt2 = VariableType::from(c2.as_ref());

                    vt1.merge(&vt2)
                }
            }
            (VariableType::Object(o1), VariableType::Object(o2)) => {
                let cap = o1.capacity().max(o2.capacity());

                let map = o1.iter().chain(o2.iter()).fold(
                    HashMap::<String, Rc<VariableType>>::with_capacity(cap),
                    |mut acc, (k, v)| {
                        match acc.entry(k.clone()) {
                            Entry::Occupied(mut occ) => {
                                let current = occ.get();
                                let merged = v.merge(current.as_ref());
                                occ.insert(Rc::new(merged));
                            }
                            Entry::Vacant(vac) => {
                                vac.insert(v.clone());
                            }
                        }

                        acc
                    },
                );

                VariableType::Object(map)
            }
            (_, _) => VariableType::Any,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::variable::VariableType;
    use std::rc::Rc;

    #[test]
    fn merge_simple() {
        assert_eq!(
            VariableType::Number.merge(&VariableType::Number),
            VariableType::Number
        );
        assert_eq!(
            VariableType::String.merge(&VariableType::String),
            VariableType::String
        );
        assert_eq!(
            VariableType::Bool.merge(&VariableType::Bool),
            VariableType::Bool
        );
        assert_eq!(
            VariableType::Null.merge(&VariableType::Null),
            VariableType::Null
        );
        assert_eq!(
            VariableType::Any.merge(&VariableType::Any),
            VariableType::Any
        );
    }

    #[test]
    fn merge_array() {
        assert_eq!(
            VariableType::Array(Rc::new(VariableType::Number))
                .merge(&VariableType::Array(Rc::new(VariableType::Number))),
            VariableType::Array(Rc::new(VariableType::Number))
        );
    }

    #[test]
    fn merge_mixed() {
        assert_eq!(
            VariableType::Number.merge(&VariableType::String),
            VariableType::Any
        );
    }
}

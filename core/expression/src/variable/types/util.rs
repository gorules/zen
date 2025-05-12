use crate::variable::types::VariableType;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::rc::Rc;

impl VariableType {
    pub fn iterator(&self) -> Option<Rc<VariableType>> {
        match self {
            VariableType::Array(item) => Some(item.clone()),
            VariableType::Interval => Some(Rc::new(VariableType::Number)),
            _ => None,
        }
    }

    pub fn as_const_str(&self) -> Option<Rc<str>> {
        match self {
            VariableType::Const(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Rc<VariableType> {
        match self {
            VariableType::Object(obj) => {
                obj.get(key).cloned().unwrap_or(Rc::new(VariableType::Any))
            }
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
            (VariableType::Date, VariableType::Date) => true,
            (VariableType::Number, VariableType::Date) => true,
            (_, VariableType::Date) if self.widen().is_string() => true,
            (VariableType::Interval, VariableType::Interval) => true,
            (VariableType::Array(a1), VariableType::Array(a2)) => a1.satisfies(a2),
            (VariableType::Object(o1), VariableType::Object(o2)) => o1
                .iter()
                .all(|(k, v)| o2.get(k).is_some_and(|tv| v.satisfies(tv))),

            (VariableType::Const(c1), VariableType::Const(c2)) => c1 == c2,
            (VariableType::Const(c), VariableType::Enum(_, e)) => e.iter().any(|e| e == c),
            (VariableType::Const(_), VariableType::String) => true,
            (VariableType::String, VariableType::Const(_)) => true,

            (VariableType::Enum(_, e1), VariableType::Enum(_, e2)) => {
                e1.iter().all(|c| e2.contains(c))
            }
            (VariableType::Enum(_, e), VariableType::Const(c)) => e.iter().all(|i| i == c),
            (VariableType::Enum(_, _), VariableType::String) => true,
            (VariableType::String, VariableType::Enum(_, _)) => true,

            (_, _) => false,
        }
    }

    pub fn is_array(&self) -> bool {
        match self {
            VariableType::Any | VariableType::Array(_) => true,
            _ => false,
        }
    }

    pub fn is_iterable(&self) -> bool {
        match self {
            VariableType::Any | VariableType::Interval | VariableType::Array(_) => true,
            _ => false,
        }
    }

    pub fn is_string(&self) -> bool {
        match self {
            VariableType::String => true,
            _ => false,
        }
    }

    pub fn is_object(&self) -> bool {
        match self {
            VariableType::Any | VariableType::Object(_) => true,
            _ => false,
        }
    }

    pub fn is_null(&self) -> bool {
        match self {
            VariableType::Null => true,
            _ => false,
        }
    }

    pub fn widen(&self) -> Self {
        match self {
            VariableType::Const(_) | VariableType::Enum(_, _) => VariableType::String,
            _ => self.clone(),
        }
    }

    pub fn merge(&self, other: &Self) -> Self {
        match (self, other) {
            (VariableType::Any, _) | (_, VariableType::Any) => VariableType::Any,
            (VariableType::Null, VariableType::Null) => VariableType::Null,
            (VariableType::Bool, VariableType::Bool) => VariableType::Bool,
            (VariableType::String, VariableType::String) => VariableType::String,
            (VariableType::Number, VariableType::Number) => VariableType::Number,
            (VariableType::Date, VariableType::Date) => VariableType::Date,
            (VariableType::Interval, VariableType::Interval) => VariableType::Interval,
            (VariableType::Array(a1), VariableType::Array(a2)) => {
                if Rc::ptr_eq(a1, a2) {
                    VariableType::Array(a1.clone())
                } else {
                    VariableType::Array(Rc::new(a1.as_ref().merge(a2.as_ref())))
                }
            }

            (VariableType::Object(o1), VariableType::Object(o2)) => {
                let mut merged = HashMap::with_capacity(o1.len().max(o2.len()));
                for (k, v) in o1.iter() {
                    merged.insert(k.clone(), v.clone());
                }

                for (k, v) in o2.iter() {
                    match merged.entry(k.clone()) {
                        Entry::Occupied(mut entry) => {
                            let current = entry.get();
                            let merged_value = current.as_ref().merge(v.as_ref());
                            entry.insert(Rc::new(merged_value));
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(v.clone());
                        }
                    }
                }

                VariableType::Object(merged)
            }

            (VariableType::Const(c), VariableType::Enum(_, values)) => {
                let mut merged = values.clone();
                if !merged.contains(c) {
                    merged.push(c.clone());
                }

                VariableType::Enum(None, merged)
            }
            (VariableType::Const(c1), VariableType::Const(c2)) => {
                if Rc::ptr_eq(c1, c2) || c1 == c2 {
                    VariableType::Const(c1.clone())
                } else {
                    VariableType::Enum(None, vec![c1.clone(), c2.clone()])
                }
            }
            (VariableType::Const(_), VariableType::String)
            | (VariableType::String, VariableType::Const(_)) => VariableType::String,

            (VariableType::Enum(n1, a), VariableType::Enum(n2, b)) => {
                let mut merged = a.clone();
                for val in b {
                    if !merged.contains(val) {
                        merged.push(val.clone());
                    }
                }

                let name = match (n1, n2) {
                    (Some(n1), Some(n2)) => Some(Rc::<str>::from(format!("{} | {}", n1, n2))),
                    _ => None,
                };

                VariableType::Enum(name, merged)
            }

            (VariableType::Enum(_, values), VariableType::Const(c)) => {
                let mut merged = values.clone();
                if !merged.contains(c) {
                    merged.push(c.clone());
                }
                VariableType::Enum(None, merged)
            }

            (VariableType::Enum(_, _), VariableType::String)
            | (VariableType::String, VariableType::Enum(_, _)) => VariableType::String,

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

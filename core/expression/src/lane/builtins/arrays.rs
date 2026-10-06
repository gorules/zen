use crate::lane::builtins::{Apply, Arg, Array, Builtin, Fail, Outcome, Params};
use crate::variable::{Variable, VariableMap};

pub(crate) struct Arrays;

impl Arrays {
    const ARRAY: &'static [(&'static str, bool)] = &[("array", false)];

    pub(crate) const FLATTEN: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Array| {
                Ok(a.with(|values| {
                    let mut flat = Vec::with_capacity(values.len());
                    for v in values {
                        match v {
                            Variable::Array(inner) => flat.extend(inner.borrow().iter().cloned()),
                            v => flat.push(v.clone()),
                        }
                    }
                    Variable::from_array(flat)
                }))
            })
        }],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    pub(crate) const MERGE: Builtin = Builtin {
        overloads: &[|args| Apply::one(args, |a: Array| a.with(Self::merge))],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    fn merge(values: &[Variable]) -> Result<Variable, Fail> {
        let Some(first) = values.iter().find(|item| !matches!(item, Variable::Null)) else {
            return Ok(Variable::empty_object());
        };
        let capacity = values
            .iter()
            .map(|item| match item {
                Variable::Object(o) => o.borrow().len(),
                Variable::Array(a) => a.borrow().len(),
                _ => 0,
            })
            .sum();
        match first {
            Variable::Array(_) => {
                let mut merged = Vec::with_capacity(capacity);
                for item in values {
                    match item {
                        Variable::Array(inner) => merged.extend(inner.borrow().iter().cloned()),
                        Variable::Null => {}
                        _ => return Err("Expected array of arrays".to_string()),
                    }
                }
                Ok(Variable::from_array(merged))
            }
            Variable::Object(_) => {
                let mut merged = VariableMap::with_capacity(capacity);
                for item in values {
                    match item {
                        Variable::Object(o) => {
                            for (key, value) in o.borrow().iter() {
                                merged.insert(key.clone(), value.clone());
                            }
                        }
                        Variable::Null => {}
                        _ => return Err("Expected array of objects".to_string()),
                    }
                }
                Ok(Variable::from_object(merged))
            }
            other => Err(format!(
                "merge expects an array of arrays or objects, got {}",
                other.type_name()
            )),
        }
    }

    pub(crate) const MERGE_DEEP: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: Array| {
                a.with(|values| {
                    let mut result = Variable::empty_object();
                    for item in values {
                        match item {
                            Variable::Object(_) => result = Self::deep(&result, item),
                            Variable::Null => {}
                            _ => return Err("Expected array of objects".to_string()),
                        }
                    }
                    Ok(result)
                })
            })
        }],
        fail: |args| Params::fail(args, Self::ARRAY),
    };

    fn deep(base: &Variable, patch: &Variable) -> Variable {
        match (base, patch) {
            (Variable::Object(a), Variable::Object(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                let mut merged = VariableMap::with_capacity(a.len() + b.len());
                for (key, value) in a.iter() {
                    merged.insert(key.clone(), value.clone());
                }
                for (key, value) in b.iter() {
                    let entry = merged
                        .get(key)
                        .map(|existing| Self::deep(existing, value))
                        .unwrap_or_else(|| value.clone());
                    merged.insert(key.clone(), entry);
                }
                Variable::from_object(merged)
            }
            (Variable::Array(a), Variable::Array(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                let mut merged = Vec::with_capacity(a.len() + b.len());
                merged.extend(a.iter().cloned());
                merged.extend(b.iter().cloned());
                Variable::from_array(merged)
            }
            (_, patch) => patch.clone(),
        }
    }

    pub(crate) const KEYS: Builtin = Builtin {
        overloads: &[Self::keys],
        fail: |_| "Argument on 0 position out of bounds".to_string(),
    };

    fn keys(args: &[Arg]) -> Outcome {
        Apply::one(args, |a: Arg| match a {
            Arg::Var(Variable::Array(_)) | Arg::List(_) => {
                let len = match a {
                    Arg::List(l) => l.end - l.start,
                    Arg::Var(Variable::Array(v)) => v.borrow().len(),
                    _ => 0,
                };
                Ok(Variable::from_array(
                    (0..len).map(|i| Variable::Number(i.into())).collect(),
                ))
            }
            Arg::Var(Variable::Object(o)) => Ok(Variable::from_array(
                o.borrow()
                    .iter()
                    .map(|(key, _)| Variable::String(key.as_str().into()))
                    .collect(),
            )),
            other => Err(format!(
                "Cannot determine keys of type {}",
                other.type_name()
            )),
        })
    }

    pub(crate) const VALUES: Builtin = Builtin {
        overloads: &[|args| {
            Apply::one(args, |a: &Variable| match a {
                Variable::Object(o) => {
                    Ok(Variable::from_array(o.borrow().values().cloned().collect()))
                }
                _ => Err("Argument on 0 is not a object".to_string()),
            })
        }],
        fail: |args| Params::fail(args, &[("object", false)]),
    };
}

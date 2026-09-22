mod conv;
mod util;

use ahash::HashMap;
pub use ahash::HashMapExt as VariableMapExt;
use serde::ser::{SerializeMap, SerializeTupleVariant};
use serde::{Deserialize, Serialize, Serializer};
use std::cell::RefCell;
use std::fmt::{Display, Write};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

type RcCell<T> = Rc<RefCell<T>>;

/// Object nesting kept when serialising; deeper (or cyclic) objects are emitted empty.
pub const MAX_TYPE_DEPTH: usize = 32;

#[derive(Clone, Eq, PartialEq, Deserialize)]
pub enum VariableType {
    Any,
    Null,
    Bool,
    String,
    Number,
    Date,
    Interval,
    Array(Rc<VariableType>),
    Object(RcCell<HashMap<Rc<str>, VariableType>>),

    Const(Rc<str>),
    Enum(Option<Rc<str>>, Vec<Rc<str>>),
    Nullable(Rc<VariableType>),
}

impl VariableType {
    pub fn array(self) -> Self {
        Self::Array(Rc::new(self))
    }
}

const TYPE_NAME: &str = "VariableType";

struct Guarded<'a> {
    inner: &'a VariableType,
    path: &'a [*const ()],
}

struct GuardedFields<'a> {
    fields: &'a HashMap<Rc<str>, VariableType>,
    path: Vec<*const ()>,
}

impl Serialize for VariableType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Guarded {
            inner: self,
            path: &[],
        }
        .serialize(serializer)
    }
}

impl Serialize for Guarded<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.inner {
            VariableType::Any => serializer.serialize_unit_variant(TYPE_NAME, 0, "Any"),
            VariableType::Null => serializer.serialize_unit_variant(TYPE_NAME, 1, "Null"),
            VariableType::Bool => serializer.serialize_unit_variant(TYPE_NAME, 2, "Bool"),
            VariableType::String => serializer.serialize_unit_variant(TYPE_NAME, 3, "String"),
            VariableType::Number => serializer.serialize_unit_variant(TYPE_NAME, 4, "Number"),
            VariableType::Date => serializer.serialize_unit_variant(TYPE_NAME, 5, "Date"),
            VariableType::Interval => serializer.serialize_unit_variant(TYPE_NAME, 6, "Interval"),
            VariableType::Array(inner) => serializer.serialize_newtype_variant(
                TYPE_NAME,
                7,
                "Array",
                &Guarded {
                    inner,
                    path: self.path,
                },
            ),
            VariableType::Object(obj) => {
                let ptr = Rc::as_ptr(obj) as *const ();
                let cut = self.path.len() >= MAX_TYPE_DEPTH || self.path.contains(&ptr);
                let empty = HashMap::default();
                let borrowed;
                let fields: &HashMap<Rc<str>, VariableType> = if cut {
                    &empty
                } else {
                    borrowed = obj.borrow();
                    &borrowed
                };
                let mut path = self.path.to_vec();
                path.push(ptr);
                serializer.serialize_newtype_variant(
                    TYPE_NAME,
                    8,
                    "Object",
                    &GuardedFields { fields, path },
                )
            }
            VariableType::Const(c) => {
                serializer.serialize_newtype_variant(TYPE_NAME, 9, "Const", c.as_ref())
            }
            VariableType::Enum(name, values) => {
                let mut tv = serializer.serialize_tuple_variant(TYPE_NAME, 10, "Enum", 2)?;
                tv.serialize_field(name)?;
                tv.serialize_field(values)?;
                tv.end()
            }
            VariableType::Nullable(inner) => serializer.serialize_newtype_variant(
                TYPE_NAME,
                11,
                "Nullable",
                &Guarded {
                    inner,
                    path: self.path,
                },
            ),
        }
    }
}

impl Serialize for GuardedFields<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for (key, value) in self.fields.iter() {
            map.serialize_entry(
                key.as_ref(),
                &Guarded {
                    inner: value,
                    path: &self.path,
                },
            )?;
        }
        map.end()
    }
}

impl Default for VariableType {
    fn default() -> Self {
        VariableType::Null
    }
}

impl std::fmt::Debug for VariableType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(
            &Guarded {
                inner: self,
                path: &[],
            },
            f,
        )
    }
}

impl std::fmt::Debug for Guarded<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner {
            VariableType::Any => f.write_str("Any"),
            VariableType::Null => f.write_str("Null"),
            VariableType::Bool => f.write_str("Bool"),
            VariableType::String => f.write_str("String"),
            VariableType::Number => f.write_str("Number"),
            VariableType::Date => f.write_str("Date"),
            VariableType::Interval => f.write_str("Interval"),
            VariableType::Array(inner) | VariableType::Nullable(inner) => {
                let name = if matches!(self.inner, VariableType::Array(_)) {
                    "Array"
                } else {
                    "Nullable"
                };
                f.debug_tuple(name)
                    .field(&Guarded {
                        inner,
                        path: self.path,
                    })
                    .finish()
            }
            VariableType::Const(value) => f.debug_tuple("Const").field(value).finish(),
            VariableType::Enum(name, values) => {
                f.debug_tuple("Enum").field(name).field(values).finish()
            }
            VariableType::Object(obj) => {
                let ptr = Rc::as_ptr(obj) as *const ();
                if self.path.len() >= MAX_TYPE_DEPTH || self.path.contains(&ptr) {
                    return f.write_str("Object(<recursive>)");
                }
                let mut path = self.path.to_vec();
                path.push(ptr);
                f.debug_tuple("Object")
                    .field(&DebugCell(GuardedFields {
                        fields: &obj.borrow(),
                        path,
                    }))
                    .finish()
            }
        }
    }
}

struct DebugCell<'a>(GuardedFields<'a>);

impl std::fmt::Debug for DebugCell<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefCell").field("value", &self.0).finish()
    }
}

impl std::fmt::Debug for GuardedFields<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.fields.iter().map(|(key, value)| {
                (
                    key,
                    Guarded {
                        inner: value,
                        path: &self.path,
                    },
                )
            }))
            .finish()
    }
}

impl Display for VariableType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableType::Any => write!(f, "any"),
            VariableType::Null => write!(f, "null"),
            VariableType::Bool => write!(f, "bool"),
            VariableType::String => write!(f, "string"),
            VariableType::Number => write!(f, "number"),
            VariableType::Date => write!(f, "date"),
            VariableType::Interval => write!(f, "interval"),
            VariableType::Const(c) => write!(f, "\"{c}\""),
            VariableType::Enum(name, e) => {
                if let Some(name) = name {
                    return name.fmt(f);
                }

                let mut first = true;
                for s in e.iter() {
                    if !first {
                        f.write_str(" | ")?;
                    }

                    f.write_char('"')?;
                    f.write_str(s)?;
                    f.write_char('"')?;
                    first = false;
                }

                Ok(())
            }
            VariableType::Array(v) => write!(f, "{v}[]"),
            VariableType::Object(_) => write!(f, "object"),
            VariableType::Nullable(inner) => write!(f, "{inner}?"),
        }
    }
}

impl Hash for VariableType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self {
            VariableType::Any => 0.hash(state),
            VariableType::Null => 1.hash(state),
            VariableType::Bool => 2.hash(state),
            VariableType::String => 3.hash(state),
            VariableType::Number => 4.hash(state),
            VariableType::Date => 5.hash(state),
            VariableType::Interval => 6.hash(state),
            VariableType::Const(c) => {
                7.hash(state);
                c.hash(state)
            }
            VariableType::Enum(name, e) => {
                8.hash(state);
                name.hash(state);
                e.hash(state)
            }
            VariableType::Array(arr) => {
                9.hash(state);
                arr.hash(state)
            }
            VariableType::Object(obj) => {
                10.hash(state);

                let obj = obj.borrow();
                let mut pairs: Vec<_> = obj.iter().collect();
                pairs.sort_by_key(|i| i.0);

                Hash::hash(&pairs, state);
            }
            VariableType::Nullable(inner) => {
                11.hash(state);
                inner.hash(state);
            }
        }
    }
}

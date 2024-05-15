use bumpalo::Bump;
use serde::{Serialize, Serializer};

use crate::variable::{ser, Variable};

impl<'arena> Serialize for Variable<'arena> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Variable::Null => serializer.serialize_unit(),
            Variable::Bool(v) => serializer.serialize_bool(*v),
            Variable::Number(v) => ser::Serialize::serialize(v, serializer),
            Variable::String(v) => serializer.serialize_str(v),
            Variable::Array(v) => serializer.collect_seq(v.iter()),
            Variable::Object(v) => serializer.collect_map(v.iter()),
        }
    }
}

#[allow(dead_code)]
pub struct VariableSerializer<'arena> {
    arena: &'arena Bump,
}

impl<'arena> VariableSerializer<'arena> {
    #[allow(dead_code)]
    pub fn new_in(arena: &'arena Bump) -> Self {
        Self { arena }
    }
}

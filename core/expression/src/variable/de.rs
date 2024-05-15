use std::fmt::Formatter;
use std::marker::PhantomData;

use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use serde::Deserializer;

use crate::variable::map::BumpMap;
use crate::variable::Variable;

struct VariableVisitor<'arena> {
    arena: &'arena Bump,
}

impl<'arena, 'de: 'arena> Visitor<'de> for VariableVisitor<'arena> {
    type Value = Variable<'arena>;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("A valid type")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Bool(v))
    }

    // TODO: Error safety
    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Number(Decimal::from_i64(v).unwrap()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Number(Decimal::from_u64(v).unwrap()))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Number(Decimal::from_f64(v).unwrap()))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        match Decimal::from_str_exact(v) {
            Ok(d) => Ok(Variable::Number(d)),
            Err(_) => Ok(Variable::String(self.arena.alloc_str(v))),
        }
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut vec = BumpVec::with_capacity_in(seq.size_hint().unwrap_or_default(), self.arena);
        while let Some(value) = seq.next_element_seed(VariableDeserializer { arena: self.arena })? {
            vec.push(value);
        }

        Ok(Variable::Array(vec))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut m = BumpMap::with_capacity_in(map.size_hint().unwrap_or_default(), self.arena);
        while let Some((key, value)) =
            map.next_entry_seed(PhantomData, VariableDeserializer { arena: self.arena })?
        {
            m.insert(&*self.arena.alloc_str(key), value);
        }

        Ok(Variable::Object(m))
    }
}

pub struct VariableDeserializer<'arena> {
    arena: &'arena Bump,
}

impl<'arena> VariableDeserializer<'arena> {
    #[allow(dead_code)]
    pub fn new_in(arena: &'arena Bump) -> Self {
        Self { arena }
    }
}

impl<'arena, 'de: 'arena> DeserializeSeed<'de> for VariableDeserializer<'arena> {
    type Value = Variable<'arena>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(VariableVisitor { arena: self.arena })
    }
}

use crate::variable::Variable;
use ahash::{HashMap, HashMapExt};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt::Formatter;
use std::marker::PhantomData;
use std::ops::Deref;
use std::rc::Rc;

struct VariableVisitor;

pub(super) const NUMBER_TOKEN: &str = "$serde_json::private::Number";

impl<'de> Visitor<'de> for VariableVisitor {
    type Value = Variable;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("A valid type")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Number(Decimal::from_i64(v).ok_or_else(|| {
            Error::invalid_value(Unexpected::Signed(v), &self)
        })?))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Number(Decimal::from_u64(v).ok_or_else(|| {
            Error::invalid_value(Unexpected::Unsigned(v), &self)
        })?))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::Number(Decimal::from_f64(v).ok_or_else(|| {
            Error::invalid_value(Unexpected::Float(v), &self)
        })?))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(Variable::String(Rc::from(v)))
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
        let mut vec = Vec::with_capacity(seq.size_hint().unwrap_or_default());
        while let Some(value) = seq.next_element_seed(VariableDeserializer)? {
            vec.push(value);
        }

        Ok(Variable::from_array(vec))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut m = HashMap::with_capacity(map.size_hint().unwrap_or_default());
        let mut first = true;

        while let Some((key, value)) =
            map.next_entry_seed(PhantomData::<Rc<str>>, VariableDeserializer)?
        {
            if first && key.deref() == NUMBER_TOKEN {
                let str = value
                    .as_str()
                    .ok_or_else(|| Error::custom("failed to deserialize number"))?;

                return Ok(Variable::Number(
                    Decimal::from_str_exact(str)
                        .or_else(|_| Decimal::from_scientific(str))
                        .map_err(|_| Error::custom("invalid number"))?,
                ));
            }

            m.insert(key, value);
            first = false;
        }

        Ok(Variable::from_object(m))
    }
}

pub struct VariableDeserializer;

impl<'de> DeserializeSeed<'de> for VariableDeserializer {
    type Value = Variable;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(VariableVisitor)
    }
}

impl<'de> Deserialize<'de> for Variable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(VariableVisitor)
    }
}

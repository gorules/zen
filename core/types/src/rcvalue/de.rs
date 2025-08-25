use crate::constant::NUMBER_TOKEN;
use crate::rcvalue::RcValue;
use ahash::{HashMap, HashMapExt};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt::Formatter;
use std::marker::PhantomData;
use std::ops::Deref;
use std::rc::Rc;

struct RcValueVisitor;

impl<'de> Visitor<'de> for RcValueVisitor {
    type Value = RcValue;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        formatter.write_str("A valid type")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(RcValue::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(RcValue::Number(Decimal::from_i64(v).ok_or_else(|| {
            Error::invalid_value(Unexpected::Signed(v), &self)
        })?))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(RcValue::Number(Decimal::from_u64(v).ok_or_else(|| {
            Error::invalid_value(Unexpected::Unsigned(v), &self)
        })?))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(RcValue::Number(Decimal::from_f64(v).ok_or_else(|| {
            Error::invalid_value(Unexpected::Float(v), &self)
        })?))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(RcValue::String(Rc::from(v)))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: Error,
    {
        Ok(RcValue::Null)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut vec = Vec::with_capacity(seq.size_hint().unwrap_or_default());
        while let Some(value) = seq.next_element_seed(RcValueDeserializer)? {
            vec.push(value);
        }

        Ok(RcValue::Array(vec))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut m = HashMap::with_capacity(map.size_hint().unwrap_or_default());
        let mut first = true;

        while let Some((key, value)) =
            map.next_entry_seed(PhantomData::<Rc<str>>, RcValueDeserializer)?
        {
            if first && key.deref() == NUMBER_TOKEN {
                let str = match &value {
                    RcValue::String(s) => s.as_ref(),
                    _ => return Err(Error::custom("failed to deserialize number")),
                };

                return Ok(RcValue::Number(
                    Decimal::from_str_exact(str)
                        .or_else(|_| Decimal::from_scientific(str))
                        .map_err(|_| Error::custom("invalid number"))?,
                ));
            }

            m.insert(key, value);
            first = false;
        }

        Ok(RcValue::Object(m))
    }
}

pub struct RcValueDeserializer;

impl<'de> DeserializeSeed<'de> for RcValueDeserializer {
    type Value = RcValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(RcValueVisitor)
    }
}

impl<'de> Deserialize<'de> for RcValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(RcValueVisitor)
    }
}

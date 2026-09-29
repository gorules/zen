use rust_decimal::Decimal;
use serde::Serializer;

#[cfg(feature = "arbitrary_precision")]
use crate::constant::NUMBER_TOKEN;
#[cfg(not(feature = "arbitrary_precision"))]
use rust_decimal::prelude::ToPrimitive;
#[cfg(feature = "arbitrary_precision")]
use serde::ser::SerializeStruct;

pub(crate) struct NumberSer;

impl NumberSer {
    #[cfg(feature = "arbitrary_precision")]
    pub(crate) fn serialize<S: Serializer>(
        value: &Decimal,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct(NUMBER_TOKEN, 1)?;
        s.serialize_field(NUMBER_TOKEN, &value.normalize().to_string())?;
        s.end()
    }

    #[cfg(not(feature = "arbitrary_precision"))]
    pub(crate) fn serialize<S: Serializer>(
        value: &Decimal,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if value.is_integer() {
            if let Some(u) = value.to_u64() {
                return serializer.serialize_u64(u);
            }
            if let Some(i) = value.to_i64() {
                return serializer.serialize_i64(i);
            }
        }
        match value.to_f64() {
            Some(f) => serializer.serialize_f64(f),
            None => Err(serde::ser::Error::custom("cannot convert to f64")),
        }
    }
}

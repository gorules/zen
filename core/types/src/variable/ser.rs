use crate::variable::Variable;
#[cfg(not(feature = "arbitrary_precision"))]
use rust_decimal::prelude::ToPrimitive;
use serde::{Serialize, Serializer};

#[cfg(feature = "arbitrary_precision")]
use crate::constant::NUMBER_TOKEN;
#[cfg(feature = "arbitrary_precision")]
use serde::ser::SerializeStruct;

impl Serialize for Variable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Variable::Null => serializer.serialize_unit(),
            Variable::Bool(v) => serializer.serialize_bool(*v),
            #[cfg(feature = "arbitrary_precision")]
            Variable::Number(v) => {
                let str = v.normalize().to_string();

                let mut s = serializer.serialize_struct(NUMBER_TOKEN, 1)?;
                s.serialize_field(NUMBER_TOKEN, &str)?;
                s.end()
            }
            #[cfg(not(feature = "arbitrary_precision"))]
            Variable::Number(v) => {
                if v.scale() == 0 {
                    if let Some(i) = v.to_i64() {
                        return serializer.serialize_i64(i);
                    }
                    if let Some(u) = v.to_u64() {
                        return serializer.serialize_u64(u);
                    }
                }
                let f = v
                    .to_f64()
                    .ok_or_else(|| serde::ser::Error::custom("cannot convert to f64"))?;
                serializer.serialize_f64(f)
            }
            Variable::String(v) => serializer.serialize_str(v),
            Variable::Array(v) => {
                let borrowed = v.borrow();
                serializer.collect_seq(borrowed.iter())
            }
            Variable::Object(v) => {
                let borrowed = v.borrow();
                serializer.collect_map(borrowed.iter())
            }
            Variable::Dynamic(d) => serializer.serialize_str(d.to_string().as_str()),
        }
    }
}

use crate::variable::Variable;
use rust_decimal::prelude::ToPrimitive;
use serde::{ser, Serialize, Serializer};

impl Serialize for Variable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Variable::Null => serializer.serialize_unit(),
            Variable::Bool(v) => serializer.serialize_bool(*v),
            Variable::Number(v) => {
                if let Some(float) = v.to_f64() {
                    return serializer.serialize_f64(float);
                }

                if let Some(integer) = v.to_i128() {
                    return serializer.serialize_i128(integer);
                }

                Err(ser::Error::custom(format!(
                    "failed to serialize number: {:?}",
                    v
                )))
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
        }
    }
}

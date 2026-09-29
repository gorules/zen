use crate::number::NumberSer;
use crate::rcvalue::RcValue;
use serde::{Serialize, Serializer};

impl Serialize for RcValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            RcValue::Null => serializer.serialize_unit(),
            RcValue::Bool(v) => serializer.serialize_bool(*v),
            RcValue::Number(v) => NumberSer::serialize(v, serializer),
            RcValue::String(v) => serializer.serialize_str(v),
            RcValue::Array(v) => serializer.collect_seq(v.iter()),
            RcValue::Object(v) => serializer.collect_map(v.iter()),
        }
    }
}

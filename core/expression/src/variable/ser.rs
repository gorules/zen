use crate::variable::Variable;
use serde::{ser, Serialize, Serializer};

impl Serialize for Variable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Variable::Null => serializer.serialize_unit(),
            Variable::Bool(v) => serializer.serialize_bool(*v),
            Variable::Number(v) => ser::Serialize::serialize(v, serializer),
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

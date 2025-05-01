use crate::variable::de::NUMBER_TOKEN;
use crate::variable::Variable;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

impl Serialize for Variable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Variable::Null => serializer.serialize_unit(),
            Variable::Bool(v) => serializer.serialize_bool(*v),
            Variable::Number(v) => {
                let str = v.normalize().to_string();

                let mut s = serializer.serialize_struct(NUMBER_TOKEN, 1)?;
                s.serialize_field(NUMBER_TOKEN, &str)?;
                s.end()
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

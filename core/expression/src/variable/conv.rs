use bumpalo::collections::Vec as BumpVec;
use bumpalo::Bump;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde_json::Value;

use crate::variable::map::BumpMap;
use crate::variable::Variable;

pub trait ToVariable<'arena> {
    type Error;

    fn to_variable(&self, arena: &'arena Bump) -> Result<Variable<'arena>, Self::Error>;
}

impl<'arena> ToVariable<'arena> for Value {
    type Error = ();

    fn to_variable(&self, arena: &'arena Bump) -> Result<Variable<'arena>, Self::Error> {
        match self {
            Value::Null => Ok(Variable::Null),
            Value::Bool(v) => Ok(Variable::Bool(*v)),
            Value::Number(n) => {
                #[cfg(feature = "arbitrary_precision")]
                {
                    Decimal::from_str_exact(n.as_str()).map_err(|_| ())
                        .map(Variable::Number)
                }

                #[cfg(not(feature = "arbitrary_precision"))]
                {

                    let decimal = match n.as_u64() {
                        Some(n) => Decimal::from_u64(n).ok_or(())?,
                        None => match n.as_i64() {
                            Some(n) => Decimal::from(n),
                            None => match n.as_f64() {
                                Some(n) => Decimal::from_f64(n).ok_or(())?,
                                None => return Err(()),
                            },
                        },
                    };

                    Ok(Variable::Number(decimal))
                }
            },
            Value::String(s) => Ok(Variable::String(arena.alloc_str(s.as_str()))),
            Value::Array(arr) => {
                let mut vec = BumpVec::with_capacity_in(arr.len(), arena);
                for x in arr {
                    vec.push(x.to_variable(arena)?);
                }

                Ok(Variable::Array(vec))
            }
            Value::Object(obj) => {
                let mut m = BumpMap::with_capacity_in(obj.len(), arena);
                for (k, v) in obj {
                    m.insert(&*arena.alloc_str(k.as_str()), v.to_variable(arena)?);
                }

                Ok(Variable::Object(m))
            }
        }
    }
}

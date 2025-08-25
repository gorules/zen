mod conv;
mod de;
mod ser;

use ahash::HashMap;
pub use de::RcValueDeserializer;
use rust_decimal::Decimal;
use std::rc::Rc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RcValue {
    #[default]
    Null,
    Bool(bool),
    Number(Decimal),
    String(Rc<str>),
    Array(Vec<RcValue>),
    Object(HashMap<Rc<str>, RcValue>),
}

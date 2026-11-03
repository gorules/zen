pub(crate) use crate::compiled::host::graph::tracer::ExpressionNodeTraceItem;
use ahash::HashMap;
use std::rc::Rc;

pub(crate) type ExpressionNodeTrace = HashMap<Rc<str>, ExpressionNodeTraceItem>;

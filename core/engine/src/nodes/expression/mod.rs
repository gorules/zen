pub use crate::decision_graph::tracer::ExpressionNodeTraceItem;
use crate::model::ExpressionNodeContent;
use ahash::HashMap;
#[cfg(feature = "driver")]
pub use handler::ExpressionNodeHandler;
use std::rc::Rc;

#[cfg(feature = "driver")]
mod handler;

pub type ExpressionNodeData = ExpressionNodeContent;
pub type ExpressionNodeTrace = HashMap<Rc<str>, ExpressionNodeTraceItem>;

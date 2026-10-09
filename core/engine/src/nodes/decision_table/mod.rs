pub use crate::decision_graph::tracer::{DecisionTableNodeTrace, DecisionTableRowTrace};
#[cfg(feature = "driver")]
pub use handler::DecisionTableNodeHandler;
use zen_types::decision::DecisionTableContent;

#[cfg(feature = "driver")]
mod handler;
pub(crate) mod index;

pub type DecisionTableNodeData = DecisionTableContent;

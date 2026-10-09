#[cfg(feature = "driver")]
pub use handler::DecisionNodeHandler;
use zen_types::decision::DecisionNodeContent;
use zen_types::variable::Variable;

#[cfg(feature = "driver")]
mod handler;

pub type DecisionNodeData = DecisionNodeContent;
pub type DecisionNodeTrace = Variable;

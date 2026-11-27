pub(crate) use crate::nodes::{NodeError, NodeResponse, NodeResult};
use crate::EvaluationError;
use std::sync::Arc;
use zen_expression::variable::Variable;

impl NodeError {
    pub(crate) fn new(node_id: Arc<str>, source: impl Into<Box<dyn std::error::Error>>) -> Self {
        Self::traced(node_id, None, source)
    }

    pub(crate) fn traced(
        node_id: Arc<str>,
        trace: Option<Variable>,
        source: impl Into<Box<dyn std::error::Error>>,
    ) -> Self {
        Self {
            node_id,
            trace,
            source: source.into(),
        }
    }

    pub(crate) fn failure(self) -> EvaluationError {
        EvaluationError::NodeError {
            node_id: self.node_id,
            trace: self.trace,
            source: self.source,
        }
    }
}

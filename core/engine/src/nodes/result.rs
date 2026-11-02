use crate::EvaluationError;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use thiserror::Error;
use zen_expression::variable::Variable;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeResponse {
    pub output: Variable,
    pub trace_data: Option<Variable>,
}

pub type NodeResult = Result<NodeResponse, NodeError>;

#[derive(Debug, Error)]
pub struct NodeError {
    pub node_id: Arc<str>,
    pub trace: Option<Variable>,
    pub source: Box<dyn std::error::Error>,
}

impl NodeError {
    pub fn new(node_id: Arc<str>, source: impl Into<Box<dyn std::error::Error>>) -> Self {
        Self::traced(node_id, None, source)
    }

    pub fn traced(
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
}

impl From<NodeError> for EvaluationError {
    fn from(error: NodeError) -> Self {
        EvaluationError::NodeError {
            node_id: error.node_id,
            trace: error.trace,
            source: error.source,
        }
    }
}

impl Display for NodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

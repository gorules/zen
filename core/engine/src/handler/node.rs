use crate::model::DecisionNode;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use zen_expression::variable::Variable;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeResponse {
    pub output: Variable,
    pub trace_data: Option<Variable>,
}

#[derive(Debug, Serialize, Clone)]
pub struct NodeRequest {
    pub input: Variable,
    pub iteration: u8,
    pub node: Arc<DecisionNode>,
}

pub type NodeResult = Result<NodeResponse, NodeError>;

#[derive(Debug)]
pub enum NodeError {
    Internal,
    Other(Box<dyn std::error::Error>),
    Display(String), // For non-Error types that implement Display
    Node {
        node_id: String,
        trace: Option<Variable>,
        source: Box<NodeError>,
    },
    PartialTrace {
        trace: Option<Variable>,
        message: String,
    },
}

impl NodeError {
    /// Convert any error type to NodError
    pub fn from_error<E: std::error::Error + Send + Sync + 'static>(error: E) -> Self {
        Self::Other(Box::new(error))
    }

    /// Add context to this error
    pub fn context<C: std::fmt::Display>(self, context: C) -> Self {
        Self::Display(format!("{}: {}", context, self))
    }

    /// Add context to this error using a closure
    pub fn with_context<C: std::fmt::Display, F: FnOnce() -> C>(self, f: F) -> Self {
        Self::Display(format!("{}: {}", f(), self))
    }
}

impl Display for NodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::Internal => write!(f, "Internal error occurred"),
            NodeError::Other(err) => write!(f, "{}", err),
            NodeError::Display(msg) => write!(f, "{}", msg),
            NodeError::Node { source, .. } => {
                write!(f, "{}", source)
            }
            NodeError::PartialTrace { message, .. } => {
                write!(f, "{}", message)
            }
        }
    }
}

impl From<anyhow::Error> for Box<NodeError> {
    fn from(value: anyhow::Error) -> Self {
        Box::new(NodeError::Other(value.into()))
    }
}

impl From<anyhow::Error> for NodeError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value.into())
    }
}

impl From<String> for NodeError {
    fn from(value: String) -> Self {
        Self::Display(value)
    }
}

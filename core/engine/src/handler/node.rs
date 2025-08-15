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

#[derive(Debug)]
pub struct NodeError {
    pub node_id: String,
    pub trace: Option<Variable>,
    pub source: NodError,
}

impl Display for NodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Debug)]
pub(crate) struct PartialTraceError {
    pub trace: Option<Variable>,
    pub message: String,
}

impl Display for PartialTraceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub type NodeResult = Result<NodeResponse, NodError>;

#[derive(Debug)]
pub enum NodError {
    Internal,
    Other(Box<dyn std::error::Error>),
    Display(String), // For non-Error types that implement Display
    Node {
        node_id: String,
        trace: Option<Variable>,
        source: Box<NodError>,
    },
    PartialTrace {
        trace: Option<Variable>,
        message: String,
    },
}

impl NodError {
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

impl Display for NodError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            NodError::Internal => write!(f, "Internal error occurred"),
            NodError::Other(err) => write!(f, "{}", err),
            NodError::Display(msg) => write!(f, "{}", msg),
            NodError::Node { source, .. } => {
                write!(f, "{}", source)
            }
            NodError::PartialTrace { trace, message } => {
                if let Some(var) = trace {
                    write!(f, "{} (trace: {:?})", message, var)
                } else {
                    write!(f, "{}", message)
                }
            }
        }
    }
}

impl From<anyhow::Error> for NodError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value.into())
    }
}

impl From<String> for NodError {
    fn from(value: String) -> Self {
        Self::Display(value)
    }
}

impl From<NodeError> for NodError {
    fn from(value: NodeError) -> Self {
        Self::Node {
            source: Box::new(value.source),
            trace: value.trace.clone(),
            node_id: value.node_id,
        }
    }
}

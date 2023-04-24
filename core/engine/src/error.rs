use crate::handler::node::NodeError;
use crate::loader::LoaderError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("Loader error")]
    LoaderError(Box<LoaderError>),

    #[error("Node error")]
    NodeError(Box<NodeError>),

    #[error("Depth limit exceeded")]
    DepthLimitExceeded,

    #[error("Node not found {0}")]
    NodeConnectError(String),
}

impl From<LoaderError> for Box<EvaluationError> {
    fn from(error: LoaderError) -> Self {
        Box::new(EvaluationError::LoaderError(error.into()))
    }
}

impl From<Box<LoaderError>> for Box<EvaluationError> {
    fn from(error: Box<LoaderError>) -> Self {
        Box::new(EvaluationError::LoaderError(error))
    }
}

impl From<NodeError> for Box<EvaluationError> {
    fn from(error: NodeError) -> Self {
        Box::new(EvaluationError::NodeError(error.into()))
    }
}

impl From<Box<NodeError>> for Box<EvaluationError> {
    fn from(error: Box<NodeError>) -> Self {
        Box::new(EvaluationError::NodeError(error))
    }
}

use crate::handler::graph::DecisionGraphValidationError;
use crate::handler::node::NodeError;
use crate::loader::LoaderError;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("Loader error")]
    LoaderError(Box<LoaderError>),

    #[error("Node error")]
    NodeError(Box<NodeError>),

    #[error("Depth limit exceeded")]
    DepthLimitExceeded,

    #[error("Invalid graph")]
    InvalidGraph(Box<DecisionGraphValidationError>),
}

impl Serialize for EvaluationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;
        match self {
            EvaluationError::DepthLimitExceeded => {
                map.serialize_entry("type", "DepthLimitExceeded")?;
            }
            EvaluationError::NodeError(err) => {
                map.serialize_entry("type", "NodeError")?;
                map.serialize_entry("nodeId", &err.node_id)?;
                map.serialize_entry("source", &err.source.to_string())?;
            }
            EvaluationError::LoaderError(err) => {
                map.serialize_entry("type", "LoaderError")?;
                match err.as_ref() {
                    LoaderError::Internal { key, source } => {
                        map.serialize_entry("key", key)?;
                        map.serialize_entry("source", &source.to_string())?;
                    }
                    LoaderError::NotFound(key) => {
                        map.serialize_entry("key", key)?;
                    }
                }
            }
            EvaluationError::InvalidGraph(err) => {
                map.serialize_entry("type", "InvalidGraph")?;
                map.serialize_entry("source", err)?;
            }
        }

        map.end()
    }
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

impl From<DecisionGraphValidationError> for Box<EvaluationError> {
    fn from(error: DecisionGraphValidationError) -> Self {
        Box::new(EvaluationError::InvalidGraph(error.into()))
    }
}

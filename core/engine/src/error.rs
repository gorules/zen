use crate::decision_graph::graph::DecisionGraphValidationError;
use crate::engine::EvaluationTraceKind;
use crate::loader::LoaderError;
use crate::nodes::NodeError;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("Loader error")]
    LoaderError(LoaderError),

    #[error("{0}")]
    NodeError(NodeError),

    #[error("Depth limit exceeded")]
    DepthLimitExceeded,

    #[error("Invalid graph")]
    InvalidGraph(DecisionGraphValidationError),

    #[error("Validation failed")]
    Validation(Value),
}

impl EvaluationError {
    pub fn serialize_with_mode<S>(
        &self,
        serializer: S,
        mode: EvaluationTraceKind,
    ) -> Result<S::Ok, S::Error>
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
                map.serialize_entry("source", &err.source.to_string())?;
                map.serialize_entry("nodeId", &err.node_id)?;

                if let Some(trace) = &err.trace {
                    map.serialize_entry("trace", &mode.serialize_trace(trace))?;
                }
            }
            EvaluationError::LoaderError(err) => {
                map.serialize_entry("type", "LoaderError")?;
                match err {
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
            EvaluationError::Validation(err) => {
                map.serialize_entry("type", "Validation")?;
                map.serialize_entry("source", err)?;
            }
        }

        map.end()
    }
}

impl Serialize for EvaluationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.serialize_with_mode(serializer, Default::default())
    }
}

impl From<LoaderError> for Box<EvaluationError> {
    fn from(error: LoaderError) -> Self {
        Box::new(EvaluationError::LoaderError(error.into()))
    }
}

impl From<NodeError> for Box<EvaluationError> {
    fn from(value: NodeError) -> Self {
        Box::new(EvaluationError::NodeError(value))
    }
}

impl From<DecisionGraphValidationError> for Box<EvaluationError> {
    fn from(error: DecisionGraphValidationError) -> Self {
        Box::new(EvaluationError::InvalidGraph(error.into()))
    }
}

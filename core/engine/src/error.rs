use crate::engine::EvaluationTraceKind;
use crate::handler::graph::DecisionGraphValidationError;
pub use crate::handler::node::NodeError;
use crate::loader::LoaderError;
use jsonschema::{ErrorIterator, ValidationError};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};
use std::iter::once;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("Loader error")]
    LoaderError(LoaderError),

    #[error("Node error")]
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

                match err {
                    NodeError::Internal => map.serialize_entry("source", "Internal")?,
                    NodeError::Other(o) => map.serialize_entry("source", &o.to_string())?,
                    NodeError::Display(d) => map.serialize_entry("source", d.as_str())?,
                    NodeError::Node {
                        node_id,
                        source,
                        trace,
                    } => {
                        map.serialize_entry("nodeId", node_id.as_str())?;
                        map.serialize_entry("source", &source.to_string())?;
                        if let Some(trace) = &trace {
                            map.serialize_entry("trace", &mode.serialize_trace(trace))?;
                        }
                    }
                    NodeError::PartialTrace { trace, message } => {
                        map.serialize_entry("source", message.as_str())?;
                        if let Some(trace) = &trace {
                            map.serialize_entry("trace", &mode.serialize_trace(trace))?;
                        }
                    }
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidationErrorJson {
    path: String,
    message: String,
}

impl<'a> From<ValidationError<'a>> for ValidationErrorJson {
    fn from(value: ValidationError<'a>) -> Self {
        ValidationErrorJson {
            path: value.instance_path.to_string(),
            message: format!("{}", value),
        }
    }
}

impl<'a> From<ErrorIterator<'a>> for Box<EvaluationError> {
    fn from(error_iter: ErrorIterator<'a>) -> Self {
        let errors: Vec<ValidationErrorJson> = error_iter.into_iter().map(From::from).collect();

        let mut json_map = Map::new();
        json_map.insert(
            "errors".to_string(),
            serde_json::to_value(errors).unwrap_or_default(),
        );

        Box::new(EvaluationError::Validation(Value::Object(json_map)))
    }
}

impl<'a> From<ValidationError<'a>> for Box<EvaluationError> {
    fn from(value: ValidationError<'a>) -> Self {
        let iterator: ErrorIterator<'a> = Box::new(once(value));
        Box::<EvaluationError>::from(iterator)
    }
}

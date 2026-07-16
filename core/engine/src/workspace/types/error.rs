use std::fmt;
use std::sync::Arc;

use serde::ser::SerializeMap;
use thiserror::Error;
use zen_expression::IsolateError;

#[derive(Debug, Clone)]
pub struct InputValidationError {
    pub path: String,
    pub expected: String,
    pub got: String,
}

impl fmt::Display for InputValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}': expected {}, got {}",
            self.path, self.expected, self.got
        )
    }
}

#[derive(Debug, Error)]
pub enum EvaluationError {
    #[error("policy '{0}' not found in workspace")]
    PolicyNotFound(Arc<str>),

    #[error("document '{0}' is a graph; evaluate it through the decision engine")]
    GraphNotEvaluable(Arc<str>),

    #[error("policy '{policy_path}' imports '{import}' which is not in the workspace")]
    ImportNotFound {
        policy_path: Arc<str>,
        import: Arc<str>,
    },

    #[error("policy '{policy_path}' has compilation errors; cannot evaluate")]
    CompilationErrors { policy_path: Arc<str> },

    #[error("goal property '{0}' not found in policy")]
    GoalNotFound(Arc<str>),

    #[error(
        "missing required inputs [{}] for goals [{}]",
        FormatList(.missing),
        FormatList(.goals)
    )]
    MissingRequiredInputs {
        goals: Vec<Arc<str>>,
        missing: Vec<Arc<str>>,
    },

    #[error("input validation failed: {}", FormatList(.errors))]
    InputValidationFailed { errors: Vec<InputValidationError> },

    #[error(
        "expression `{expression}` failed in block '{block_id}' (policy '{policy_path}'): {source}"
    )]
    ExpressionFailed {
        policy_path: Arc<str>,
        block_id: Arc<str>,
        expression: Arc<str>,
        source: IsolateError,
        partial_trace: Option<Box<crate::workspace::types::Trace>>,
    },
}

impl EvaluationError {
    pub fn serialize_into_map<M: SerializeMap>(&self, map: &mut M) -> Result<(), M::Error> {
        match self {
            Self::PolicyNotFound(path) => {
                map.serialize_entry("kind", "PolicyNotFound")?;
                map.serialize_entry("policyPath", path)?;
            }
            Self::GraphNotEvaluable(path) => {
                map.serialize_entry("kind", "GraphNotEvaluable")?;
                map.serialize_entry("policyPath", path)?;
            }
            Self::ImportNotFound {
                policy_path,
                import,
            } => {
                map.serialize_entry("kind", "ImportNotFound")?;
                map.serialize_entry("policyPath", policy_path)?;
                map.serialize_entry("import", import)?;
            }
            Self::CompilationErrors { policy_path } => {
                map.serialize_entry("kind", "CompilationErrors")?;
                map.serialize_entry("policyPath", policy_path)?;
            }
            Self::GoalNotFound(name) => {
                map.serialize_entry("kind", "GoalNotFound")?;
                map.serialize_entry("goal", name)?;
            }
            Self::MissingRequiredInputs { goals, missing } => {
                map.serialize_entry("kind", "MissingRequiredInputs")?;
                map.serialize_entry("goals", goals)?;
                map.serialize_entry("missing", missing)?;
            }
            Self::InputValidationFailed { errors } => {
                map.serialize_entry("kind", "InputValidationFailed")?;
                let messages: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
                map.serialize_entry("errors", &messages)?;
            }
            Self::ExpressionFailed {
                policy_path,
                block_id,
                expression,
                source,
                partial_trace,
            } => {
                map.serialize_entry("kind", "ExpressionFailed")?;
                map.serialize_entry("policyPath", policy_path)?;
                map.serialize_entry("blockId", block_id)?;
                map.serialize_entry("expression", expression)?;
                map.serialize_entry("source", &source.to_string())?;
                if let Some(trace) = partial_trace {
                    map.serialize_entry("trace", trace)?;
                }
            }
        }
        Ok(())
    }
}

struct FormatList<'a, T>(&'a [T]);

impl<T: fmt::Display> fmt::Display for FormatList<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, item) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{item}")?;
        }
        Ok(())
    }
}

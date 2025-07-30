use serde_json::json;
use std::fmt::Formatter;
use zen_expression::IsolateError;

#[allow(dead_code)]
#[derive(Debug, uniffi::Error)]
pub enum ZenError {
    Zero,

    InvalidArgument,
    StringNullError,
    StringUtf8Error,
    JsonSerializationFailed,
    JsonDeserializationFailed,
    ExecutionTaskSpawnError,

    IsolateError(String),
    EvaluationError(String),
    ValidationError(String),

    LoaderKeyNotFound { key: String },
    LoaderInternalError { key: String, details: String },

    TemplateEngineError { template: String, details: String },
}

impl ZenError {
    pub fn details(&self) -> String {
        match &self {
            ZenError::IsolateError(error) => error.to_string(),
            ZenError::EvaluationError(error) => error.to_string(),
            ZenError::ValidationError(error) => error.to_string(),
            ZenError::LoaderKeyNotFound { key } => json!({ "key": key }).to_string(),
            ZenError::LoaderInternalError { key, details } => {
                json!({ "key": key, "details": details }).to_string()
            }
            ZenError::TemplateEngineError { template, details } => {
                json!({ "template": template, "details": details }).to_string()
            }
            ZenError::Zero => String::from("Zero"),
            ZenError::InvalidArgument => String::from("InvalidArgument"),
            ZenError::StringNullError => String::from("StringNullError"),
            ZenError::StringUtf8Error => String::from("StringUtf8Error"),
            ZenError::JsonSerializationFailed => String::from("JsonSerializationFailed"),
            ZenError::JsonDeserializationFailed => String::from("JsonDeserializationFailed"),
            ZenError::ExecutionTaskSpawnError => String::from("ExecutionTaskSpawnError"),
        }
    }
}

impl std::fmt::Display for ZenError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.details().fmt(f)
    }
}

impl From<IsolateError> for ZenError {
    fn from(error: IsolateError) -> Self {
        ZenError::EvaluationError(
            serde_json::to_string(&error).unwrap_or_else(|_| error.to_string()),
        )
    }
}

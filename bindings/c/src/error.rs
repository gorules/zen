use serde_json::{json, Value};
use strum::{EnumDiscriminants, FromRepr};

#[allow(dead_code)]
#[derive(EnumDiscriminants)]
#[strum_discriminants(derive(FromRepr))]
#[repr(u8)]
pub enum ZenError {
    Zero,

    InvalidArgument,
    StringNullError,
    StringUtf8Error,
    JsonSerializationFailed,
    JsonDeserializationFailed,

    IsolateError(Value),
    EvaluationError(Value),

    LoaderKeyNotFound { key: String },
    LoaderInternalError { key: String, message: String },
}

impl ZenError {
    pub fn details(&self) -> Option<String> {
        let json = match &self {
            ZenError::IsolateError(error) => Some(error.clone()),
            ZenError::EvaluationError(error) => Some(error.clone()),
            ZenError::LoaderKeyNotFound { key } => Some(json!({ "key": key })),
            ZenError::LoaderInternalError { key, message } => {
                Some(json!({ "key": key, "message": message }))
            }
            _ => None,
        };

        json.map(|j| j.to_string())
    }
}

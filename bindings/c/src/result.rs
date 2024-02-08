use std::ffi::CString;
use std::ptr::null_mut;

use libc::c_char;

use zen_engine::loader::LoaderError;
use zen_engine::EvaluationError;
use zen_expression::IsolateError;

use crate::error::{ZenError, ZenErrorDiscriminants};

/// CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
/// manually. Instead, use error or ok functions for initialisation.
#[repr(C)]
pub struct ZenResult<T> {
    result: *mut T,
    error: ZenErrorDiscriminants,
    details: *mut c_char,
}

impl<T> ZenResult<T> {
    pub(crate) fn error(error: ZenError) -> Self {
        let details = match error.details() {
            Some(data) => {
                let Ok(cstring_data) = CString::new(data) else {
                    return Self {
                        error: ZenErrorDiscriminants::StringNullError,
                        result: null_mut(),
                        details: null_mut(),
                    };
                };

                cstring_data.into_raw()
            }
            None => null_mut(),
        };

        return Self {
            result: null_mut(),
            error: ZenErrorDiscriminants::from(error),
            details,
        };
    }

    pub(crate) fn ok(result: *mut T) -> Self {
        return Self {
            result,
            error: ZenErrorDiscriminants::Zero,
            details: null_mut(),
        };
    }
}

impl<T> From<&Box<EvaluationError>> for ZenResult<T> {
    fn from(evaluation_error: &Box<EvaluationError>) -> Self {
        let Ok(value) = serde_json::to_value(evaluation_error) else {
            return ZenResult::error(ZenError::JsonSerializationFailed);
        };

        ZenResult::error(ZenError::EvaluationError(value))
    }
}

impl<T> From<&IsolateError> for ZenResult<T> {
    fn from(isolate_error: &IsolateError) -> Self {
        let Ok(value) = serde_json::to_value(isolate_error) else {
            return ZenResult::error(ZenError::JsonSerializationFailed);
        };

        ZenResult::error(ZenError::IsolateError(value))
    }
}

impl<T> From<&Box<LoaderError>> for ZenResult<T> {
    fn from(loader_error: &Box<LoaderError>) -> Self {
        match loader_error.as_ref() {
            LoaderError::NotFound(key) => {
                ZenResult::error(ZenError::LoaderKeyNotFound { key: key.clone() })
            }
            LoaderError::Internal { source, key } => {
                ZenResult::error(ZenError::LoaderInternalError {
                    key: key.clone(),
                    message: source.to_string(),
                })
            }
        }
    }
}

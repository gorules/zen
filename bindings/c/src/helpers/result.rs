use libc::c_char;
use std::ffi::CString;
use std::ptr::{null, null_mut};
use zen_engine::loader::LoaderError;
use zen_engine::EvaluationError;

/// CResult can be seen as Either<Result, Error>. It cannot, and should not, be initialized
/// manually. Instead, use error or ok functions for initialisation.
#[repr(C)]
pub struct CResult<T> {
    result: *mut T,
    error: *const c_char,
}

impl<T> CResult<T> {
    pub(crate) fn error<E: Into<Vec<u8>>>(err: E) -> Self {
        return Self {
            result: null_mut(),
            error: CString::new(err).unwrap().into_raw(),
        };
    }

    pub(crate) fn ok(result: *mut T) -> Self {
        return Self {
            result,
            error: null(),
        };
    }
}

/// Normalise error by unwrapping nested internal LoaderError, as we want to give languages full control
impl<T> From<&Box<EvaluationError>> for CResult<T> {
    fn from(evaluation_error: &Box<EvaluationError>) -> Self {
        let error = match evaluation_error.as_ref() {
            EvaluationError::LoaderError(le) => return CResult::from(le),
            _ => evaluation_error.to_string(),
        };

        Self {
            result: null_mut(),
            error: CString::new(error).unwrap().into_raw(),
        }
    }
}

impl<T> From<&Box<LoaderError>> for CResult<T> {
    fn from(loader_error: &Box<LoaderError>) -> Self {
        let error = match loader_error.as_ref() {
            LoaderError::Internal { source, .. } => source.to_string(),
            _ => loader_error.to_string(),
        };

        Self {
            result: null_mut(),
            error: CString::new(error).unwrap().into_raw(),
        }
    }
}

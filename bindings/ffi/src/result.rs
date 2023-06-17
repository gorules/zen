use libc::c_char;
use std::ffi::CString;
use std::ptr::{null, null_mut};

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

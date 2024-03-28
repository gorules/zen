use std::ffi::CStr;

use libc::c_char;

pub(crate) fn safe_cstr_from_ptr<'a>(ptr: *const c_char) -> Option<&'a CStr> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: The caller must ensure the pointer is not null and points to a valid, null-terminated C string.
        // This unsafe block is necessary because CStr::from_ptr inherently requires an unsafe operation.
        Some(unsafe { CStr::from_ptr(ptr) })
    }
}

pub(crate) fn safe_str_from_ptr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: The caller must ensure the pointer is not null and points to a valid, null-terminated C string.
        // This unsafe block is necessary because CStr::from_ptr inherently requires an unsafe operation.
        Some(unsafe { CStr::from_ptr(ptr) }.to_str().ok()?)
    }
}

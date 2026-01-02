extern crate zen_engine;

mod custom_node;
mod decision;
mod engine;
mod error;
mod expression;
mod helper;
mod languages;
mod loader;
mod mt;
mod result;

use std::ffi::{c_char, c_int, c_void, CString};

/// Allocates a string using Rust's allocator.
/// The caller must free the returned pointer using zen_free_string.
/// Returns null if the input is null or if allocation fails.
#[no_mangle]
pub extern "C" fn zen_alloc_string(ptr: *const c_char, len: usize) -> *mut c_char {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    match CString::new(slice) {
        Ok(cstring) => cstring.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Frees a string that was allocated by Rust.
/// This must be called for any string returned by zen_* functions to avoid memory leaks.
/// This is safe to call with a null pointer.
#[no_mangle]
pub extern "C" fn zen_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe {
            let _ = CString::from_raw(ptr);
        }
    }
}

/// Frees an integer pointer that was allocated by Rust.
/// This must be called for ZenResult<c_int> result field when it's not null.
#[no_mangle]
pub extern "C" fn zen_free_int(ptr: *mut c_int) {
    if !ptr.is_null() {
        let _ = unsafe { Box::from_raw(ptr) };
    }
}

/// Generic free function for any Rust-allocated memory.
/// Use zen_free_string for strings returned by Rust functions.
#[no_mangle]
pub extern "C" fn zen_free(ptr: *mut c_void) {
    if !ptr.is_null() {
        let _ = unsafe { Box::from_raw(ptr as *mut u8) };
    }
}

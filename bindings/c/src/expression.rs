use std::ffi::{CStr, CString};

use libc::{c_char, c_int};

use zen_expression::{evaluate_expression, evaluate_unary_expression};

use crate::error::ZenError;
use crate::result::ZenResult;

/// Evaluate expression, responsible for freeing expression and context
#[no_mangle]
pub extern "C" fn zen_evaluate_expression(
    expression: *const c_char,
    context: *const c_char,
) -> ZenResult<c_char> {
    if expression.is_null() || context.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let Ok(expression_str) = unsafe { CStr::from_ptr(expression) }.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context_str) = unsafe { CStr::from_ptr(context) }.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context_val) = serde_json::from_str(context_str) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let maybe_result = evaluate_expression(expression_str, &context_val);
    let result = match maybe_result {
        Ok(r) => r,
        Err(err) => return ZenResult::from(&err),
    };

    let Ok(string_result) = serde_json::to_string(&result) else {
        return ZenResult::error(ZenError::JsonSerializationFailed);
    };

    let result = unsafe { CString::from_vec_unchecked(string_result.into_bytes()) };
    ZenResult::ok(result.into_raw())
}

/// Evaluate unary expression, responsible for freeing expression and context
/// True = 1
/// False = 0
#[no_mangle]
pub extern "C" fn zen_evaluate_unary_expression(
    expression: *const c_char,
    context: *const c_char,
) -> ZenResult<c_int> {
    if expression.is_null() || context.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let Ok(expression_str) = unsafe { CStr::from_ptr(expression) }.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context_str) = unsafe { CStr::from_ptr(context) }.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context_val) = serde_json::from_str(context_str) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let maybe_result = evaluate_unary_expression(expression_str, &context_val);
    let result = match maybe_result {
        Ok(r) => r,
        Err(err) => return ZenResult::from(&err),
    };

    let c_result: i32 = if result { 1 } else { 0 };

    ZenResult::ok(Box::into_raw(Box::new(c_result)))
}

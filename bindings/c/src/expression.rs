use std::ffi::CString;

use libc::{c_char, c_int};

use zen_expression::{evaluate_expression, evaluate_unary_expression};

use crate::error::ZenError;
use crate::helper::{safe_cstr_from_ptr, safe_str_from_ptr};
use crate::result::ZenResult;

#[no_mangle]
pub extern "C" fn zen_evaluate_expression(
    expression: *const c_char,
    context: *const c_char,
) -> ZenResult<c_char> {
    let Some(expression_str) = safe_str_from_ptr(expression) else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Some(context_cstr) = safe_cstr_from_ptr(context) else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context_val) = serde_json::from_slice(context_cstr.to_bytes()) else {
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
    let Some(expression_str) = safe_str_from_ptr(expression) else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Some(context_cstr) = safe_cstr_from_ptr(context) else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context_val) = serde_json::from_slice(context_cstr.to_bytes()) else {
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

/// Evaluate unary expression, responsible for freeing expression and context
/// True = 1
/// False = 0
#[no_mangle]
pub extern "C" fn zen_evaluate_template(
    template: *const c_char,
    context: *const c_char,
) -> ZenResult<c_char> {
    let Some(template_str) = safe_str_from_ptr(template) else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Some(context_cstr) = safe_cstr_from_ptr(context) else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context_val) = serde_json::from_slice(context_cstr.to_bytes()) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let result = match zen_template::render(template_str, &context_val) {
        Ok(r) => r,
        Err(err) => {
            return ZenResult::error(ZenError::TemplateEngineError {
                message: err.to_string(),
                template: template_str.to_string(),
            })
        }
    };

    let Ok(s_result) = serde_json::to_string(&result) else {
        return ZenResult::error(ZenError::JsonSerializationFailed);
    };

    let c_result = unsafe { CString::from_vec_unchecked(s_result.into_bytes()) };
    ZenResult::ok(c_result.into_raw())
}

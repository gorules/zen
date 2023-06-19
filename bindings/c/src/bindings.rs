use crate::helpers::result::CResult;
use crate::helpers::types::{
    CZenDecision, CZenDecisionEngine, CZenDecisionEnginePtr, CZenDecisionPtr,
    CZenEngineEvaluationOptions, DynDecisionLoader,
};
use crate::loader::{CDecisionLoader, CZenDecisionLoaderCallback};

use futures::executor::block_on;
use serde_json::Value;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::Arc;
use zen_engine::loader::NoopLoader;
use zen_engine::model::DecisionContent;
use zen_engine::DecisionEngine;

/// Create a new DecisionEngine instance, caller is responsible for freeing the returned reference
#[no_mangle]
pub extern "C" fn zen_engine_new() -> *mut CZenDecisionEnginePtr {
    let loader = Arc::new(NoopLoader::default());
    let engine: CZenDecisionEngine = DecisionEngine::new(DynDecisionLoader::new(loader));

    Box::into_raw(Box::new(engine)) as *mut CZenDecisionEnginePtr
}

/// Creates a new DecisionEngine instance with loader, caller is responsible for freeing the returned reference
#[no_mangle]
pub extern "C" fn zen_engine_new_with_loader(
    callback: CZenDecisionLoaderCallback,
) -> *mut CZenDecisionEnginePtr {
    let loader = Arc::new(CDecisionLoader::new(callback));
    let engine: CZenDecisionEngine = DecisionEngine::new(DynDecisionLoader::new(loader));

    Box::into_raw(Box::new(engine)) as *mut CZenDecisionEnginePtr
}

/// Frees the DecisionEngine instance reference from the memory
#[no_mangle]
pub extern "C" fn zen_engine_free(engine: *mut CZenDecisionEnginePtr) {
    assert!(!engine.is_null());

    unsafe { Box::from_raw(engine as *mut CZenDecisionEnginePtr) };
}

/// Creates a Decision using a reference of DecisionEngine and content (JSON)
/// Caller is responsible for freeing: Decision reference (returned) and content_ptr
#[no_mangle]
pub extern "C" fn zen_engine_create_decision(
    engine_ptr: *const CZenDecisionEnginePtr,
    content_ptr: *const c_char,
) -> CResult<CZenDecisionPtr> {
    if engine_ptr.is_null() {
        return CResult::error("PTR_NULL: engine_ptr");
    }

    if content_ptr.is_null() {
        return CResult::error("PTR_NULL: content_ptr");
    }

    let engine = unsafe { &*(engine_ptr as *mut CZenDecisionEngine) };
    let cstr_content = unsafe { CStr::from_ptr(content_ptr) };

    let Ok(content) = cstr_content.to_str() else {
        return CResult::error("INVALID_STR: content_ptr");
    };

    let decision_content: DecisionContent = match serde_json::from_str(content) {
        Ok(content) => content,
        Err(e) => return CResult::error(format!("JSON_PARSE: content_ptr {}", e.to_string())),
    };

    let decision = engine.create_decision(Arc::new(decision_content));
    CResult::ok(Box::into_raw(Box::new(decision)) as *mut CZenDecisionPtr)
}

/// Evaluates rules engine using a DecisionEngine reference via loader
/// Caller is responsible for freeing: key_ptr, context_ptr and returned value
#[no_mangle]
pub extern "C" fn zen_engine_evaluate(
    engine_ptr: *const CZenDecisionEnginePtr,
    key_ptr: *const c_char,
    context_ptr: *const c_char,
    options: CZenEngineEvaluationOptions,
) -> CResult<c_char> {
    if engine_ptr.is_null() {
        return CResult::error("PTR_NULL: engine_ptr");
    }

    if key_ptr.is_null() {
        return CResult::error("PTR_NULL: key_ptr");
    }

    if context_ptr.is_null() {
        return CResult::error("PTR_NULL: context_ptr");
    }

    let engine = unsafe { &*(engine_ptr as *mut CZenDecisionEngine) };
    let cstr_key = unsafe { CStr::from_ptr(key_ptr) };
    let Ok(key) = cstr_key.to_str() else {
        return CResult::error("INVALID_STR: key_ptr")
    };

    let cstr_context = unsafe { CStr::from_ptr(context_ptr) };
    let Ok(str_context) = cstr_context.to_str() else {
        return CResult::error("INVALID_STR: context_ptr");
    };

    let context: Value = match serde_json::from_str(str_context) {
        Ok(c) => c,
        Err(e) => return CResult::error(format!("JSON_PARSE: context_ptr {}", e.to_string())),
    };

    let maybe_result = block_on(engine.evaluate_with_opts(key, &context, options.into()));
    let result = match maybe_result {
        Ok(r) => r,
        Err(e) => return CResult::from(&e),
    };

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_result.into_bytes()) };

    CResult::ok(cstring_result.into_raw())
}

/// Loads a Decision through DecisionEngine
/// Caller is responsible for freeing: key_ptr and returned Decision reference
#[no_mangle]
pub extern "C" fn zen_engine_load_decision(
    engine_ptr: *const CZenDecisionEnginePtr,
    key_ptr: *const c_char,
) -> CResult<CZenDecisionPtr> {
    if engine_ptr.is_null() {
        return CResult::error("PTR_NULL: engine_ptr");
    }

    if key_ptr.is_null() {
        return CResult::error("PTR_NULL: key_ptr");
    }

    let engine = unsafe { &*(engine_ptr as *mut CZenDecisionEngine) };
    let cstr_key = unsafe { CStr::from_ptr(key_ptr) };
    let Ok(key) = cstr_key.to_str() else {
        return CResult::error("INVALID_STR: key_ptr")
    };

    let decision = match block_on(engine.get_decision(key)) {
        Ok(d) => d,
        Err(e) => return CResult::from(&e),
    };

    CResult::ok(Box::into_raw(Box::new(decision)) as *mut c_void)
}

/// Evaluates rules engine using a Decision
/// Caller is responsible for freeing: content_ptr and returned value
#[no_mangle]
pub extern "C" fn zen_engine_decision_evaluate(
    decision_ptr: *const CZenDecisionPtr,
    context_ptr: *const c_char,
    options: CZenEngineEvaluationOptions,
) -> CResult<c_char> {
    if decision_ptr.is_null() {
        return CResult::error("PTR_NULL: decision_ptr");
    }

    if context_ptr.is_null() {
        return CResult::error("PTR_NULL: context_ptr");
    }

    let decision = unsafe { &*(decision_ptr as *mut CZenDecision) };
    let cstr_context = unsafe { CStr::from_ptr(context_ptr) };
    let Ok(str_context) = cstr_context.to_str() else {
        return CResult::error("INVALID_STR: context_ptr");
    };

    let context: Value = match serde_json::from_str(str_context) {
        Ok(c) => c,
        Err(e) => return CResult::error(format!("JSON_PARSE: context_ptr {}", e.to_string())),
    };

    let maybe_result = block_on(decision.evaluate_with_opts(&context, options.into()));
    let result = match maybe_result {
        Ok(r) => r,
        Err(e) => return CResult::from(&e),
    };

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_result.into_bytes()) };

    CResult::ok(cstring_result.into_raw())
}

#[no_mangle]
pub extern "C" fn zen_engine_decision_free(decision_ptr: *mut CZenDecisionPtr) {
    unsafe { Box::from_raw(decision_ptr as *mut CZenDecision) };
}

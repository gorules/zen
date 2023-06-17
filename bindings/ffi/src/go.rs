extern crate zen_engine;

use anyhow::anyhow;
use async_trait::async_trait;
use std::ffi::{c_void, CStr, CString};
use std::sync::Arc;

use futures::executor::block_on;
use libc::c_char;
use serde_json::Value;

use crate::result::CResult;
use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;
use zen_engine::{Decision, DecisionEngine, EvaluationOptions};

type GoDecision = Decision<GoDecisionLoader>;
type GoDecisionEngine = DecisionEngine<GoDecisionLoader>;

type CGoEngine = c_void;
type CGoDecision = c_void;

#[no_mangle]
pub extern "C" fn go_zen_engine_new(maybe_loader: Option<&usize>) -> *mut CGoEngine {
    let loader = GoDecisionLoader::new(maybe_loader.cloned());
    let engine = DecisionEngine::new(loader);

    Box::into_raw(Box::new(engine)) as *mut CGoEngine
}

#[no_mangle]
pub extern "C" fn go_zen_engine_free(engine: *mut CGoEngine) {
    assert!(!engine.is_null());

    unsafe { Box::from_raw(engine as *mut GoDecisionEngine) };
}

#[no_mangle]
pub extern "C" fn go_zen_engine_create_decision(
    engine_ptr: *const CGoEngine,
    content_ptr: *const c_char,
) -> CResult<CGoDecision> {
    if engine_ptr.is_null() {
        return CResult::error("PTR_NULL: engine_ptr");
    }

    if content_ptr.is_null() {
        return CResult::error("PTR_NULL: content_ptr");
    }

    let engine = unsafe { &*(engine_ptr as *mut GoDecisionEngine) };
    let cstr_content = unsafe { CStr::from_ptr(content_ptr) };

    let Ok(content) = cstr_content.to_str() else {
        return CResult::error("INVALID_STR: content_ptr");
    };

    let decision_content: DecisionContent = match serde_json::from_str(content) {
        Ok(content) => content,
        Err(e) => return CResult::error(format!("JSON_PARSE: content_ptr {}", e.to_string())),
    };

    let decision = engine.create_decision(Arc::new(decision_content));
    CResult::ok(Box::into_raw(Box::new(decision)) as *mut CGoDecision)
}

#[no_mangle]
pub extern "C" fn go_zen_engine_evaluate(
    engine_ptr: *const CGoEngine,
    key_ptr: *const c_char,
    context_ptr: *const c_char,
    trace: bool,
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

    let engine = unsafe { &*(engine_ptr as *mut GoDecisionEngine) };
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

    let maybe_result = block_on(engine.evaluate_with_opts(
        key,
        &context,
        EvaluationOptions {
            max_depth: Some(5),
            trace: Some(trace),
        },
    ));

    let result = match maybe_result {
        Ok(r) => r,
        Err(e) => return CResult::error(format!("EVALUATION: {}", e.to_string())),
    };

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_result.into_bytes()) };

    CResult::ok(cstring_result.into_raw())
}

#[no_mangle]
pub extern "C" fn go_zen_engine_load_decision(
    engine_ptr: *const CGoEngine,
    key_ptr: *const c_char,
) -> CResult<CGoDecision> {
    if engine_ptr.is_null() {
        return CResult::error("PTR_NULL: engine_ptr");
    }

    if key_ptr.is_null() {
        return CResult::error("PTR_NULL: key_ptr");
    }

    let engine = unsafe { &*(engine_ptr as *mut GoDecisionEngine) };
    let cstr_key = unsafe { CStr::from_ptr(key_ptr) };
    let Ok(key) = cstr_key.to_str() else {
        return CResult::error("INVALID_STR: key_ptr")
    };

    let decision = match block_on(engine.get_decision(key)) {
        Ok(d) => d,
        Err(e) => return CResult::error(format!("EVALUATION: {}", e.to_string())),
    };

    CResult::ok(Box::into_raw(Box::new(decision)) as *mut c_void)
}

#[no_mangle]
pub extern "C" fn go_zen_engine_decision_evaluate(
    decision_ptr: *const CGoDecision,
    context_ptr: *const c_char,
    trace: bool,
) -> CResult<c_char> {
    if decision_ptr.is_null() {
        return CResult::error("PTR_NULL: decision_ptr");
    }

    if context_ptr.is_null() {
        return CResult::error("PTR_NULL: context_ptr");
    }

    let decision = unsafe { &*(decision_ptr as *mut GoDecision) };
    let cstr_context = unsafe { CStr::from_ptr(context_ptr) };
    let Ok(str_context) = cstr_context.to_str() else {
        return CResult::error("INVALID_STR: context_ptr");
    };

    let context: Value = match serde_json::from_str(str_context) {
        Ok(c) => c,
        Err(e) => return CResult::error(format!("JSON_PARSE: context_ptr {}", e.to_string())),
    };

    let maybe_result = block_on(decision.evaluate_with_opts(
        &context,
        EvaluationOptions {
            max_depth: Some(5),
            trace: Some(trace),
        },
    ));

    let result = match maybe_result {
        Ok(r) => r,
        Err(e) => return CResult::error(format!("EVALUATION: {}", e.to_string())),
    };

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_result.into_bytes()) };

    CResult::ok(cstring_result.into_raw())
}

#[no_mangle]
pub extern "C" fn go_zen_engine_decision_free(decision_ptr: *mut CGoDecision) {
    unsafe { Box::from_raw(decision_ptr as *mut GoDecision) };
}

#[allow(unused_doc_comments)]
/// cbindgen:ignore
extern "C" {
    fn go_zen_engine_loader_callback(cb_ptr: usize, key: *const c_char) -> *mut c_char;
}

#[derive(Default)]
pub(crate) struct GoDecisionLoader {
    handler: Option<usize>,
}

impl GoDecisionLoader {
    pub fn new(handler: Option<usize>) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl DecisionLoader for GoDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        let Some(handler) = &self.handler else {
            return Err(LoaderError::NotFound(key.to_string()).into())
        };

        let c_key = CString::new(key).unwrap();
        let content_c_ptr =
            unsafe { go_zen_engine_loader_callback(handler.clone(), c_key.as_ptr()) };
        if content_c_ptr.is_null() {
            return Err(LoaderError::NotFound(key.to_string()).into());
        }

        let c_content = unsafe { CString::from_raw(content_c_ptr) };
        let Ok(k) = c_content.to_str() else {
            return Err(LoaderError::Internal {
                key: key.to_string(),
                source: anyhow!("Failed to cast c_content to string")
            }.into())
        };

        let decision_content: DecisionContent = match serde_json::from_str(k) {
            Ok(d) => d,
            Err(e) => {
                return Err(LoaderError::Internal {
                    key: key.to_string(),
                    source: anyhow!(e),
                }
                .into())
            }
        };

        Ok(Arc::new(decision_content))
    }
}

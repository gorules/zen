extern crate zen_engine;

use async_trait::async_trait;
use std::ffi::{c_void, CStr, CString};
use std::sync::Arc;

use futures::executor::block_on;
use libc::c_char;
use serde_json::Value;

use zen_engine::loader::{DecisionLoader, LoaderError, LoaderResponse};
use zen_engine::model::DecisionContent;
use zen_engine::{Decision, DecisionEngine, EvaluationOptions};

type GoDecision = Decision<GoDecisionLoader>;
type GoDecisionEngine = DecisionEngine<GoDecisionLoader>;

#[no_mangle]
pub extern "C" fn go_zen_engine_new(maybe_loader: Option<&usize>) -> *mut c_void {
    let loader = GoDecisionLoader::new(maybe_loader.cloned());
    let engine = DecisionEngine::new(loader);

    Box::into_raw(Box::new(engine)) as *mut c_void
}

#[no_mangle]
pub extern "C" fn go_zen_engine_free(engine: *mut c_void) {
    assert!(!engine.is_null());

    unsafe { Box::from_raw(engine as *mut GoDecisionEngine) };
}

#[no_mangle]
pub extern "C" fn go_zen_engine_create_decision(
    engine_ptr: *mut c_void,
    content_ptr: *const c_char,
) -> *mut c_void {
    assert!(!engine_ptr.is_null());

    let engine = unsafe { &*(engine_ptr as *mut GoDecisionEngine) };
    let cstr_content = unsafe { CStr::from_ptr(content_ptr) };
    let content = cstr_content.to_str().unwrap();
    let decision_content: DecisionContent = serde_json::from_str(content).unwrap();
    let decision = engine.create_decision(Arc::new(decision_content));

    Box::into_raw(Box::new(decision)) as *mut c_void
}

#[no_mangle]
pub extern "C" fn go_zen_engine_evaluate(
    engine_ptr: *mut c_void,
    key_ptr: *const c_char,
    context_ptr: *const c_char,
    trace: bool,
) -> *const c_char {
    assert!(!engine_ptr.is_null());

    let engine = unsafe { &*(engine_ptr as *mut GoDecisionEngine) };
    let cstr_key = unsafe { CStr::from_ptr(key_ptr) };
    let key = cstr_key.to_str().unwrap();
    let cstr_context = unsafe { CStr::from_ptr(context_ptr) };
    let str_context = cstr_context.to_str().unwrap();
    let context: Value = serde_json::from_str(str_context).unwrap();

    let result = block_on(engine.evaluate_with_opts(
        key,
        &context,
        EvaluationOptions {
            max_depth: Some(5),
            trace: Some(trace),
        },
    ))
    .unwrap();

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result =
        unsafe { CString::from_vec_with_nul_unchecked(serialized_result.into_bytes()) };

    cstring_result.into_raw()
}

#[no_mangle]
pub extern "C" fn go_zen_engine_load_decision(
    engine_ptr: *mut c_void,
    key_ptr: *const c_char,
) -> *mut c_void {
    assert!(!engine_ptr.is_null());

    let engine = unsafe { &*(engine_ptr as *mut GoDecisionEngine) };
    let cstr_key = unsafe { CStr::from_ptr(key_ptr) };
    let key = cstr_key.to_str().unwrap();
    let decision = block_on(engine.get_decision(key)).unwrap();

    Box::into_raw(Box::new(decision)) as *mut c_void
}

#[no_mangle]
pub extern "C" fn go_zen_engine_decision_evaluate(
    decision_ptr: *mut c_void,
    context_ptr: *const c_char,
    trace: bool,
) -> *const c_char {
    assert!(!decision_ptr.is_null());

    let decision = unsafe { &*(decision_ptr as *mut GoDecision) };
    let cstr_context = unsafe { CStr::from_ptr(context_ptr) };
    let str_context = cstr_context.to_str().unwrap();
    let context: Value = serde_json::from_str(str_context).unwrap();
    let result = block_on(decision.evaluate_with_opts(
        &context,
        EvaluationOptions {
            max_depth: Some(5),
            trace: Some(trace),
        },
    ))
    .unwrap();

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result =
        unsafe { CString::from_vec_with_nul_unchecked(serialized_result.into_bytes()) };

    cstring_result.into_raw()
}

#[no_mangle]
pub extern "C" fn go_zen_engine_decision_free(decision_ptr: *mut c_void) {
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

        let c_key = unsafe { CStr::from_bytes_with_nul_unchecked(key.as_bytes()) };
        let content_c_ptr =
            unsafe { go_zen_engine_loader_callback(handler.clone(), c_key.as_ptr()) };
        let c_content = unsafe { CStr::from_ptr(content_c_ptr) };
        let k = c_content.to_str().unwrap();
        let decision_content: DecisionContent = serde_json::from_str(k).unwrap();

        Ok(Arc::new(decision_content))
    }
}

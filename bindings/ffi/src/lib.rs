extern crate zen_engine;

use std::ffi::{CStr, CString};
use std::sync::Arc;

use futures::executor::block_on;
use libc::c_char;
use serde_json::Value;

use zen_engine::loader::NoopLoader;
use zen_engine::model::DecisionContent;
use zen_engine::{Decision, DecisionEngine};

use crate::loader::FfiLoader;

mod loader;

type LoaderFn = extern "C" fn(key: *const c_char) -> *const c_char;

#[no_mangle]
pub extern "C" fn zen_engine_new() -> *mut DecisionEngine<FfiLoader> {
    let loader = FfiLoader::default();
    let engine = DecisionEngine::new(loader);
    Box::into_raw(Box::new(engine))
}

#[no_mangle]
pub extern "C" fn zen_engine_new_with_loader(function: LoaderFn) -> *mut DecisionEngine<FfiLoader> {
    let loader = FfiLoader::new(function);
    let engine = DecisionEngine::new(loader);
    Box::into_raw(Box::new(engine))
}

#[no_mangle]
pub extern "C" fn zen_engine_free(engine: *mut DecisionEngine<FfiLoader>) {
    unsafe { Box::from_raw(engine) };
}

#[no_mangle]
pub extern "C" fn zen_engine_create_decision(
    engine_ptr: *mut DecisionEngine<FfiLoader>,
    content_ptr: *const c_char,
) -> *mut Decision<FfiLoader> {
    assert!(!engine_ptr.is_null());

    let engine = unsafe { &*engine_ptr };
    let cstr_content = unsafe { CStr::from_ptr(content_ptr) };
    let content = cstr_content.to_str().unwrap();
    let decision_content: DecisionContent = serde_json::from_str(content).unwrap();
    let decision = engine.create_decision(Arc::new(decision_content));

    Box::into_raw(Box::new(decision))
}

#[no_mangle]
pub extern "C" fn zen_engine_load_decision(
    engine_ptr: *mut DecisionEngine<FfiLoader>,
    key_ptr: *const c_char,
) -> *mut Decision<FfiLoader> {
    assert!(!engine_ptr.is_null());

    let engine = unsafe { &*engine_ptr };
    let cstr_key = unsafe { CStr::from_ptr(key_ptr) };
    let key = cstr_key.to_str().unwrap();
    let decision = block_on(engine.get_decision(key)).unwrap();

    Box::into_raw(Box::new(decision))
}

#[no_mangle]
pub extern "C" fn zen_engine_decision_evaluate(
    decision_ptr: *mut Decision<FfiLoader>,
    context_ptr: *const c_char,
) -> *const c_char {
    assert!(!decision_ptr.is_null());

    let decision = unsafe { &*decision_ptr };
    let cstr_context = unsafe { CStr::from_ptr(context_ptr) };
    let str_context = cstr_context.to_str().unwrap();
    let context: Value = serde_json::from_str(str_context).unwrap();
    let result = block_on(decision.evaluate(&context)).unwrap();

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result =
        unsafe { CString::from_vec_with_nul_unchecked(serialized_result.into_bytes()) };

    cstring_result.into_raw()
}

#[no_mangle]
pub extern "C" fn zen_engine_decision_free(decision_ptr: *mut Decision<FfiLoader>) {
    unsafe { Box::from_raw(decision_ptr) };
}

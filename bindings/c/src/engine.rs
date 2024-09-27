use serde_json::Value;
use std::ffi::{c_char, CStr, CString};
use std::marker::{PhantomData, PhantomPinned};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use zen_engine::{DecisionEngine, EvaluationOptions};

use crate::custom_node::DynamicCustomNode;
use crate::decision::{ZenDecision, ZenDecisionStruct};
use crate::error::ZenError;
use crate::helper::safe_str_from_ptr;
use crate::loader::DynamicDecisionLoader;
use crate::mt::tokio_runtime;
use crate::result::ZenResult;

pub(crate) struct ZenEngine(DecisionEngine<DynamicDecisionLoader, DynamicCustomNode>);

impl Deref for ZenEngine {
    type Target = DecisionEngine<DynamicDecisionLoader, DynamicCustomNode>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ZenEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Default for ZenEngine {
    fn default() -> Self {
        Self(DecisionEngine::new(
            Arc::new(DynamicDecisionLoader::default()),
            Arc::new(DynamicCustomNode::default()),
        ))
    }
}

impl ZenEngine {
    pub fn new(loader: DynamicDecisionLoader, custom_node: DynamicCustomNode) -> Self {
        Self(DecisionEngine::new(Arc::new(loader), Arc::new(custom_node)))
    }
}

#[repr(C)]
pub(crate) struct ZenEngineStruct {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

#[repr(C)]
pub struct ZenEngineEvaluationOptions {
    trace: bool,
    max_depth: u8,
}

impl Into<EvaluationOptions> for ZenEngineEvaluationOptions {
    fn into(self) -> EvaluationOptions {
        EvaluationOptions {
            trace: Some(self.trace),
            max_depth: Some(self.max_depth),
        }
    }
}

/// Create a new ZenEngine instance, caller is responsible for freeing the returned reference
/// by calling zen_engine_free.
#[no_mangle]
pub extern "C" fn zen_engine_new() -> *mut ZenEngineStruct {
    Box::into_raw(Box::new(ZenEngine::default())) as *mut ZenEngineStruct
}

/// Frees the ZenEngine instance reference from the memory
#[no_mangle]
pub extern "C" fn zen_engine_free(engine: *mut ZenEngineStruct) {
    if !engine.is_null() {
        let _ = unsafe { Box::from_raw(engine as *mut ZenEngine) };
    }
}

/// Creates a Decision using a reference of DecisionEngine and content (JSON)
/// Caller is responsible for freeing content and ZenResult.
#[no_mangle]
pub extern "C" fn zen_engine_create_decision(
    engine: *const ZenEngineStruct,
    content: *const c_char,
) -> ZenResult<ZenDecisionStruct> {
    if engine.is_null() || content.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let cstr_content = unsafe { CStr::from_ptr(content) };
    let Ok(decision_content) = serde_json::from_slice(cstr_content.to_bytes()) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let zen_engine = unsafe { &*(engine as *mut ZenEngine) };
    let decision = zen_engine.create_decision(Arc::new(decision_content));

    let zen_decision = ZenDecision::from(decision);
    ZenResult::ok(Box::into_raw(Box::new(zen_decision)) as *mut ZenDecisionStruct)
}

/// Evaluates rules engine using a DecisionEngine reference via loader
/// Caller is responsible for freeing: key, context and ZenResult.
#[no_mangle]
pub extern "C" fn zen_engine_evaluate(
    engine: *const ZenEngineStruct,
    key: *const c_char,
    context: *const c_char,
    options: ZenEngineEvaluationOptions,
) -> ZenResult<c_char> {
    if engine.is_null() || key.is_null() || context.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let Some(str_key) = safe_str_from_ptr(key) else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let cstr_context = unsafe { CStr::from_ptr(context) };
    let Ok(val_context) = serde_json::from_slice::<Value>(cstr_context.to_bytes()) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let zen_engine = unsafe { &*(engine as *mut ZenEngine) };

    let maybe_result = tokio_runtime().block_on(zen_engine.evaluate_with_opts(
        str_key,
        val_context.into(),
        options.into(),
    ));
    let result = match maybe_result {
        Ok(r) => r,
        Err(e) => return ZenResult::from(&e),
    };

    let Ok(serialized_result) = serde_json::to_string(&result) else {
        return ZenResult::error(ZenError::JsonSerializationFailed);
    };

    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_result.into_bytes()) };
    ZenResult::ok(cstring_result.into_raw())
}

/// Loads a Decision through DecisionEngine
/// Caller is responsible for freeing: key and ZenResult.
#[no_mangle]
pub extern "C" fn zen_engine_get_decision(
    engine: *const ZenEngineStruct,
    key: *const c_char,
) -> ZenResult<ZenDecisionStruct> {
    if engine.is_null() || key.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let cstr_key = unsafe { CStr::from_ptr(key) };
    let Ok(str_key) = cstr_key.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let zen_engine = unsafe { &*(engine as *mut ZenEngine) };
    let decision = match tokio_runtime().block_on(zen_engine.get_decision(str_key)) {
        Ok(d) => d,
        Err(e) => return ZenResult::from(&e),
    };

    let zen_decision = ZenDecision::from(decision);
    ZenResult::ok(Box::into_raw(Box::new(zen_decision)) as *mut ZenDecisionStruct)
}

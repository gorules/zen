use std::ffi::{c_char, c_void, CStr, CString};
use std::marker::{PhantomData, PhantomPinned};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use futures::executor::block_on;

use zen_engine::{DecisionEngine, EvaluationOptions};

use crate::decision::ZenDecision;
use crate::error::ZenError;
use crate::loader::DynamicDecisionLoader;
use crate::result::ZenResult;

#[repr(C)]
pub(crate) struct ZenEngine {
    _data: DecisionEngine<DynamicDecisionLoader>,
    _marker: PhantomData<(*mut c_void, PhantomPinned)>,
}

impl Deref for ZenEngine {
    type Target = DecisionEngine<DynamicDecisionLoader>;

    fn deref(&self) -> &Self::Target {
        &self._data
    }
}

impl DerefMut for ZenEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self._data
    }
}

impl Default for ZenEngine {
    fn default() -> Self {
        Self {
            _data: DecisionEngine::new(DynamicDecisionLoader::default()),
            _marker: PhantomData,
        }
    }
}

impl ZenEngine {
    pub fn with_loader(loader: DynamicDecisionLoader) -> Self {
        Self {
            _data: DecisionEngine::new(loader),
            _marker: PhantomData,
        }
    }
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
pub extern "C" fn zen_engine_new() -> *mut ZenEngine {
    Box::into_raw(Box::new(ZenEngine::default()))
}

/// Frees the ZenEngine instance reference from the memory
#[no_mangle]
pub extern "C" fn zen_engine_free(engine: *mut ZenEngine) {
    if !engine.is_null() {
        let _ = unsafe { Box::from_raw(engine) };
    }
}

/// Creates a Decision using a reference of DecisionEngine and content (JSON)
/// Caller is responsible for freeing content and ZenResult.
#[no_mangle]
pub extern "C" fn zen_engine_create_decision(
    engine: *const ZenEngine,
    content: *const c_char,
) -> ZenResult<ZenDecision> {
    if engine.is_null() || content.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let cstr_content = unsafe { CStr::from_ptr(content) };
    let Ok(str_content) = cstr_content.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(decision_content) = serde_json::from_str(str_content) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let zen_engine = unsafe { &*engine };
    let decision = zen_engine.create_decision(Arc::new(decision_content));

    ZenResult::ok(Box::into_raw(Box::new(decision.into())))
}

/// Evaluates rules engine using a DecisionEngine reference via loader
/// Caller is responsible for freeing: key, context and ZenResult.
#[no_mangle]
pub extern "C" fn zen_engine_evaluate(
    engine: *const ZenEngine,
    key: *const c_char,
    context: *const c_char,
    options: ZenEngineEvaluationOptions,
) -> ZenResult<c_char> {
    if engine.is_null() || key.is_null() || context.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let cstr_key = unsafe { CStr::from_ptr(key) };
    let Ok(str_key) = cstr_key.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let cstr_context = unsafe { CStr::from_ptr(context) };
    let Ok(str_context) = cstr_context.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(val_context) = serde_json::from_str(str_context) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let zen_engine = unsafe { &*engine };

    let maybe_result =
        block_on(zen_engine.evaluate_with_opts(str_key, &val_context, options.into()));
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
    engine: *const ZenEngine,
    key: *const c_char,
) -> ZenResult<ZenDecision> {
    if engine.is_null() || key.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let cstr_key = unsafe { CStr::from_ptr(key) };
    let Ok(str_key) = cstr_key.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let zen_engine = unsafe { &*engine };
    let decision = match block_on(zen_engine.get_decision(str_key)) {
        Ok(d) => d,
        Err(e) => return ZenResult::from(&e),
    };

    ZenResult::ok(Box::into_raw(Box::new(decision.into())))
}

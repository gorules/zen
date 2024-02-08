use std::ffi::{c_char, c_void, CStr, CString};
use std::marker::{PhantomData, PhantomPinned};
use std::ops::{Deref, DerefMut};

use futures::executor::block_on;

use zen_engine::Decision;

use crate::engine::ZenEngineEvaluationOptions;
use crate::error::ZenError;
use crate::loader::DynamicDecisionLoader;
use crate::result::ZenResult;

#[repr(C)]
pub(crate) struct ZenDecision {
    _data: Decision<DynamicDecisionLoader>,
    _marker: PhantomData<(*mut c_void, PhantomPinned)>,
}

impl Deref for ZenDecision {
    type Target = Decision<DynamicDecisionLoader>;

    fn deref(&self) -> &Self::Target {
        &self._data
    }
}

impl DerefMut for ZenDecision {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self._data
    }
}

impl From<Decision<DynamicDecisionLoader>> for ZenDecision {
    fn from(value: Decision<DynamicDecisionLoader>) -> Self {
        Self {
            _data: value,
            _marker: PhantomData,
        }
    }
}

/// Frees ZenDecision
#[no_mangle]
pub extern "C" fn zen_engine_decision_free(decision: *mut ZenDecision) {
    let _ = unsafe { Box::from_raw(decision) };
}

/// Evaluates ZenDecision
/// Caller is responsible for freeing context and ZenResult.
#[no_mangle]
pub extern "C" fn zen_decision_evaluate(
    decision: *const ZenDecision,
    context_ptr: *const c_char,
    options: ZenEngineEvaluationOptions,
) -> ZenResult<c_char> {
    if decision.is_null() || context_ptr.is_null() {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let cstr_context = unsafe { CStr::from_ptr(context_ptr) };
    let Ok(str_context) = cstr_context.to_str() else {
        return ZenResult::error(ZenError::InvalidArgument);
    };

    let Ok(context) = serde_json::from_str(str_context) else {
        return ZenResult::error(ZenError::JsonDeserializationFailed);
    };

    let zen_decision = unsafe { &*decision };
    let maybe_result = block_on(zen_decision.evaluate_with_opts(&context, options.into()));
    let result = match maybe_result {
        Ok(r) => r,
        Err(e) => return ZenResult::from(&e),
    };

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_result.into_bytes()) };

    ZenResult::ok(cstring_result.into_raw())
}

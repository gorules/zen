use std::ffi::{c_char, c_void, CStr, CString};
use std::marker::{PhantomData, PhantomPinned};
use std::ops::{Deref, DerefMut};

use zen_engine::Decision;

use crate::custom_node::DynamicCustomNode;
use crate::engine::ZenEngineEvaluationOptions;
use crate::error::ZenError;
use crate::loader::DynamicDecisionLoader;
use crate::mt::tokio_runtime;
use crate::result::ZenResult;

#[repr(C)]
pub(crate) struct ZenDecision {
    _data: Decision<DynamicDecisionLoader, DynamicCustomNode>,
    _marker: PhantomData<(*mut c_void, PhantomPinned)>,
}

impl Deref for ZenDecision {
    type Target = Decision<DynamicDecisionLoader, DynamicCustomNode>;

    fn deref(&self) -> &Self::Target {
        &self._data
    }
}

impl DerefMut for ZenDecision {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self._data
    }
}

impl From<Decision<DynamicDecisionLoader, DynamicCustomNode>> for ZenDecision {
    fn from(value: Decision<DynamicDecisionLoader, DynamicCustomNode>) -> Self {
        Self {
            _data: value,
            _marker: PhantomData,
        }
    }
}

#[repr(C)]
pub(crate) struct ZenDecisionStruct {
    _data: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Frees ZenDecision
#[no_mangle]
pub extern "C" fn zen_decision_free(decision: *mut ZenDecisionStruct) {
    let _ = unsafe { Box::from_raw(decision as *mut ZenDecision) };
}

/// Evaluates ZenDecision
/// Caller is responsible for freeing context and ZenResult.
#[no_mangle]
pub extern "C" fn zen_decision_evaluate(
    decision: *const ZenDecisionStruct,
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

    let zen_decision = unsafe { &*(decision as *mut ZenDecision) };
    let maybe_result =
        tokio_runtime().block_on(zen_decision.evaluate_with_opts(&context, options.into()));
    let result = match maybe_result {
        Ok(r) => r,
        Err(e) => return ZenResult::from(&e),
    };

    let serialized_result = serde_json::to_string(&result).unwrap();
    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_result.into_bytes()) };

    ZenResult::ok(cstring_result.into_raw())
}

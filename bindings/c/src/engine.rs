use serde_json::Value;
use std::ffi::{c_char, CStr, CString};
use std::marker::{PhantomData, PhantomPinned};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use zen_engine::{DecisionEngine, EvaluationOptions};

use crate::custom_node::DynamicCustomNode;
use crate::custom_node::ZenCustomNodeResult;
use crate::decision::{ZenDecision, ZenDecisionStruct};
use crate::error::ZenError;
use crate::helper::safe_str_from_ptr;
use crate::languages::native::NativeCustomNode;
use crate::loader::{DynamicDecisionLoader, ZenEngineLoaderConfig};
use crate::mt::{tokio_runtime, worker_pool};
use crate::result::ZenResult;
use serde_json::json;

pub(crate) struct ZenEngine(DecisionEngine);

impl Deref for ZenEngine {
    type Target = DecisionEngine;

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
            trace: self.trace,
            max_depth: self.max_depth,
        }
    }
}

/// Create a new ZenEngine instance, caller is responsible for freeing the returned reference
/// by calling zen_engine_free.
#[no_mangle]
pub extern "C" fn zen_engine_new() -> *mut ZenEngineStruct {
    Box::into_raw(Box::new(ZenEngine::default())) as *mut ZenEngineStruct
}

/// Creates a new ZenEngine instance from a loader configuration, caller is responsible for
/// freeing the returned reference by calling zen_engine_free.
#[no_mangle]
pub extern "C" fn zen_engine_new_with_loader_config(
    config: ZenEngineLoaderConfig,
    maybe_custom_node: Option<extern "C" fn(request: *const c_char) -> ZenCustomNodeResult>,
) -> ZenResult<ZenEngineStruct> {
    let loader = match config.to_dynamic_loader() {
        Ok(loader) => loader,
        Err(error) => return ZenResult::error(error),
    };

    let custom_node = match maybe_custom_node {
        Some(callback) => DynamicCustomNode::Native(NativeCustomNode::new(callback)),
        None => DynamicCustomNode::default(),
    };

    let engine = ZenEngine::new(DynamicDecisionLoader::Config(loader), custom_node);
    engine.compile();

    ZenResult::ok(Box::into_raw(Box::new(engine)) as *mut ZenEngineStruct)
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
    let decision = match zen_engine.create_decision(Arc::new(decision_content)) {
        Ok(d) => d,
        Err(_) => return ZenResult::error(ZenError::InvalidArgument),
    };

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

#[repr(C)]
pub struct ZenEngineEvaluateBatchRequest {
    key: *const c_char,
    context: *const c_char,
}

enum BatchTask {
    Failed(serde_json::Value),
    Pending(tokio::task::JoinHandle<Result<serde_json::Value, serde_json::Value>>),
}

/// Evaluates a batch of requests in parallel using a DecisionEngine reference via loader.
/// Returns a JSON array of { success, data?, error? } envelopes in request order.
/// Caller is responsible for freeing: requests and ZenResult.
#[no_mangle]
pub extern "C" fn zen_engine_evaluate_batch(
    engine: *const ZenEngineStruct,
    requests: *const ZenEngineEvaluateBatchRequest,
    requests_len: usize,
    options: ZenEngineEvaluationOptions,
) -> ZenResult<c_char> {
    if engine.is_null() || (requests.is_null() && requests_len > 0) {
        return ZenResult::error(ZenError::InvalidArgument);
    }

    let request_slice = match requests_len {
        0 => &[],
        _ => unsafe { std::slice::from_raw_parts(requests, requests_len) },
    };

    let mut parsed: Vec<(String, Result<Value, String>)> = Vec::with_capacity(requests_len);
    for request in request_slice {
        let Some(key) = safe_str_from_ptr(request.key) else {
            return ZenResult::error(ZenError::InvalidArgument);
        };

        if request.context.is_null() {
            return ZenResult::error(ZenError::InvalidArgument);
        }

        let cstr_context = unsafe { CStr::from_ptr(request.context) };
        let context =
            serde_json::from_slice::<Value>(cstr_context.to_bytes()).map_err(|e| e.to_string());
        parsed.push((key.to_string(), context));
    }

    let zen_engine = unsafe { &*(engine as *const ZenEngine) };
    let decision_engine: DecisionEngine = DecisionEngine::clone(zen_engine);
    let eval_options: EvaluationOptions = options.into();

    let pool = worker_pool();
    let tasks: Vec<BatchTask> = parsed
        .into_iter()
        .map(|(key, context)| match context {
            Err(message) => BatchTask::Failed(json!(format!("invalid context: {message}"))),
            Ok(value) => {
                let engine = decision_engine.clone();
                BatchTask::Pending(pool.spawn_pinned(move || async move {
                    engine
                        .evaluate_with_opts(key, value.into(), eval_options)
                        .await
                        .map(|response| serde_json::to_value(&response).unwrap_or(Value::Null))
                        .map_err(|e| {
                            serde_json::to_value(&e).unwrap_or_else(|_| json!(e.to_string()))
                        })
                }))
            }
        })
        .collect();

    let results = tokio_runtime().block_on(async move {
        let mut items = Vec::with_capacity(tasks.len());
        for task in tasks {
            let item = match task {
                BatchTask::Failed(error) => json!({ "success": false, "error": error }),
                BatchTask::Pending(handle) => match handle.await {
                    Ok(Ok(data)) => json!({ "success": true, "data": data }),
                    Ok(Err(error)) => json!({ "success": false, "error": error }),
                    Err(_) => {
                        json!({ "success": false, "error": "evaluation worker panicked" })
                    }
                },
            };
            items.push(item);
        }
        Value::Array(items)
    });

    let Ok(serialized_results) = serde_json::to_string(&results) else {
        return ZenResult::error(ZenError::JsonSerializationFailed);
    };

    let cstring_result = unsafe { CString::from_vec_unchecked(serialized_results.into_bytes()) };
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
        Ok(Ok(d)) => d,
        Ok(Err(_)) => return ZenResult::error(ZenError::InvalidArgument),
        Err(e) => return ZenResult::from(&e),
    };

    let zen_decision = ZenDecision::from(decision);
    ZenResult::ok(Box::into_raw(Box::new(zen_decision)) as *mut ZenDecisionStruct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ZenErrorDiscriminants;
    use crate::loader::ZenLoaderConfigKind;
    use std::ffi::CString;
    use std::ptr::null;

    fn evaluate_table(engine: *mut ZenEngineStruct) -> Value {
        let key = CString::new("table.json").unwrap();
        let context = CString::new(r#"{"input":12}"#).unwrap();
        let result = zen_engine_evaluate(
            engine,
            key.as_ptr(),
            context.as_ptr(),
            ZenEngineEvaluationOptions {
                trace: false,
                max_depth: 5,
            },
        );

        assert_eq!(result.error_code(), 0);
        let response = unsafe { CString::from_raw(result.result_ptr()) };
        serde_json::from_slice(response.to_bytes()).unwrap()
    }

    #[test]
    fn engine_from_static_loader_config() {
        let content = CString::new(format!(
            r#"{{"table.json": {}}}"#,
            include_str!("../../../test-data/table.json")
        ))
        .unwrap();

        let config = ZenEngineLoaderConfig {
            kind: ZenLoaderConfigKind::Static,
            content: content.as_ptr(),
            bytes: null(),
            bytes_len: 0,
        };

        let result = zen_engine_new_with_loader_config(config, None);
        assert_eq!(result.error_code(), 0);

        let engine = result.result_ptr();
        let response = evaluate_table(engine);
        assert_eq!(response["result"]["output"], serde_json::json!(10));

        zen_engine_free(engine);
    }

    #[test]
    fn engine_from_fs_loader_config() {
        let path = CString::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data")).unwrap();

        let config = ZenEngineLoaderConfig {
            kind: ZenLoaderConfigKind::Filesystem,
            content: path.as_ptr(),
            bytes: null(),
            bytes_len: 0,
        };

        let result = zen_engine_new_with_loader_config(config, None);
        assert_eq!(result.error_code(), 0);

        let engine = result.result_ptr();
        let response = evaluate_table(engine);
        assert_eq!(response["result"]["output"], serde_json::json!(10));

        zen_engine_free(engine);
    }

    #[test]
    fn engine_evaluate_batch_mixed_results() {
        let path = CString::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-data")).unwrap();
        let config = ZenEngineLoaderConfig {
            kind: ZenLoaderConfigKind::Filesystem,
            content: path.as_ptr(),
            bytes: null(),
            bytes_len: 0,
        };
        let engine = zen_engine_new_with_loader_config(config, None).result_ptr();

        let keys = [
            CString::new("table.json").unwrap(),
            CString::new("missing.json").unwrap(),
            CString::new("table.json").unwrap(),
        ];
        let contexts = [
            CString::new(r#"{"input":12}"#).unwrap(),
            CString::new(r#"{}"#).unwrap(),
            CString::new(r#"{"input":5}"#).unwrap(),
        ];
        let requests: Vec<ZenEngineEvaluateBatchRequest> = keys
            .iter()
            .zip(contexts.iter())
            .map(|(key, context)| ZenEngineEvaluateBatchRequest {
                key: key.as_ptr(),
                context: context.as_ptr(),
            })
            .collect();

        let result = zen_engine_evaluate_batch(
            engine,
            requests.as_ptr(),
            requests.len(),
            ZenEngineEvaluationOptions {
                trace: false,
                max_depth: 5,
            },
        );

        assert_eq!(result.error_code(), 0);
        let response = unsafe { CString::from_raw(result.result_ptr()) };
        let items: Value = serde_json::from_slice(response.to_bytes()).unwrap();

        assert_eq!(items[0]["success"], serde_json::json!(true));
        assert_eq!(items[0]["data"]["result"]["output"], serde_json::json!(10));
        assert_eq!(items[1]["success"], serde_json::json!(false));
        assert_eq!(items[2]["success"], serde_json::json!(true));
        assert_eq!(items[2]["data"]["result"]["output"], serde_json::json!(0));

        zen_engine_free(engine);
    }

    #[test]
    fn engine_from_invalid_zip_loader_config() {
        let bytes = [0u8; 4];
        let config = ZenEngineLoaderConfig {
            kind: ZenLoaderConfigKind::Zip,
            content: null(),
            bytes: bytes.as_ptr(),
            bytes_len: bytes.len(),
        };

        let result = zen_engine_new_with_loader_config(config, None);
        assert_eq!(
            result.error_code(),
            ZenErrorDiscriminants::LoaderConfigError as u8
        );
        assert!(result.result_ptr().is_null());
    }
}

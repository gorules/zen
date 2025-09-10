use napi_derive::napi;
use std::sync::atomic::Ordering;
use zen_engine::ZEN_CONFIG;

#[napi(object)]
pub struct ZenConfig {
    pub nodes_in_context: Option<bool>,
    pub function_timeout_millis: Option<u32>,
    pub http_auth: Option<bool>,
}

#[allow(dead_code)]
#[napi]
pub fn override_config(config: ZenConfig) {
    if let Some(val) = config.nodes_in_context {
        ZEN_CONFIG.nodes_in_context.store(val, Ordering::Relaxed);
    }

    if let Some(val) = config.function_timeout_millis {
        ZEN_CONFIG
            .function_timeout_millis
            .store(val as u64, Ordering::Relaxed);
    }

    if let Some(val) = config.http_auth {
        ZEN_CONFIG.http_auth.store(val, Ordering::Relaxed);
    }
}

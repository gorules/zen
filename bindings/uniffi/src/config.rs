use std::sync::atomic::Ordering;
use zen_engine::{EXPRESSION_CONFIG, ZEN_CONFIG};

#[derive(uniffi::Record)]
pub struct ZenConfig {
    pub nodes_in_context: Option<bool>,
    pub function_timeout_millis: Option<u32>,
    pub http_auth: Option<bool>,
    pub expression_max_steps: Option<u32>,
    pub expression_max_interval_length: Option<u32>,
}

#[uniffi::export]
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

    if let Some(val) = config.expression_max_steps {
        EXPRESSION_CONFIG
            .max_steps
            .store(val as u64, Ordering::Relaxed);
    }

    if let Some(val) = config.expression_max_interval_length {
        EXPRESSION_CONFIG
            .max_interval_length
            .store(val as u64, Ordering::Relaxed);
    }
}

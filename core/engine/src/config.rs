use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, AtomicU64};

#[derive(Debug)]
pub struct ZenConfig {
    pub nodes_in_context: AtomicBool,
    pub function_timeout_millis: AtomicU64,
    pub http_auth: AtomicBool,
    pub compiled: AtomicBool,
}

impl Default for ZenConfig {
    fn default() -> Self {
        Self {
            nodes_in_context: AtomicBool::new(true),
            function_timeout_millis: AtomicU64::new(5_000),
            http_auth: AtomicBool::new(true),
            compiled: AtomicBool::new(false),
        }
    }
}

pub static ZEN_CONFIG: Lazy<ZenConfig> = Lazy::new(|| Default::default());

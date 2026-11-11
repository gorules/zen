use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, AtomicU64};

#[derive(Debug)]
pub struct ZenConfig {
    pub nodes_in_context: AtomicBool,
    pub function_timeout_millis: AtomicU64,
    /// Memory limit of the JavaScript runtime used by function nodes, in bytes; 0 = no limit.
    pub function_memory_limit_bytes: AtomicU64,
    pub http_auth: AtomicBool,
}

impl Default for ZenConfig {
    fn default() -> Self {
        Self {
            nodes_in_context: AtomicBool::new(true),
            function_timeout_millis: AtomicU64::new(5_000),
            function_memory_limit_bytes: AtomicU64::new(0),
            http_auth: AtomicBool::new(true),
        }
    }
}

pub static ZEN_CONFIG: Lazy<ZenConfig> = Lazy::new(|| Default::default());

use std::sync::atomic::{AtomicBool, AtomicU64};

use once_cell::sync::Lazy;

#[derive(Debug)]
pub struct ZenConfig {
    pub nodes_in_context: AtomicBool,
    /// Function timeout presented in millis
    pub function_timeout: AtomicU64,
}

impl Default for ZenConfig {
    fn default() -> Self {
        Self {
            nodes_in_context: AtomicBool::new(true),
            function_timeout: AtomicU64::new(500),
        }
    }
}

pub static ZEN_CONFIG: Lazy<ZenConfig> = Lazy::new(|| Default::default());

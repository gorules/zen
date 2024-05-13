use once_cell::sync::Lazy;
use std::sync::atomic::AtomicBool;

#[derive(Debug)]
pub struct ZenConfig {
    pub nodes_in_context: AtomicBool,
}

impl Default for ZenConfig {
    fn default() -> Self {
        Self {
            nodes_in_context: AtomicBool::new(true),
        }
    }
}

pub static ZEN_CONFIG: Lazy<ZenConfig> = Lazy::new(|| Default::default());

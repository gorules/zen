use std::sync::atomic::Ordering;
use zen_engine::ZEN_CONFIG;

#[derive(uniffi::Record)]
pub struct ZenConfig {
    pub nodes_in_context: Option<bool>
}

#[uniffi::export]
pub fn override_config(config: ZenConfig) {
    if let Some(val) = config.nodes_in_context {
        ZEN_CONFIG.nodes_in_context.store(val, Ordering::Relaxed)
    }
}
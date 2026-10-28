use std::sync::atomic::AtomicBool;

#[derive(Debug)]
pub struct ZenExpressionConfig {
    pub lane: AtomicBool,
}

impl ZenExpressionConfig {
    const fn new() -> Self {
        Self {
            lane: AtomicBool::new(false),
        }
    }
}

impl Default for ZenExpressionConfig {
    fn default() -> Self {
        Self::new()
    }
}

pub static ZEN_EXPRESSION_CONFIG: ZenExpressionConfig = ZenExpressionConfig::new();

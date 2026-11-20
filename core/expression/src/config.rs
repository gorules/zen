//! Global limits for expression evaluation.
//!
//! Both limits default to `0`, which means "no limit" and keeps the historical behaviour. Set them once at
//! startup when expressions come from untrusted or unreviewed sources: a single expression can otherwise
//! allocate without bound (for example by materializing a huge interval) and take the whole process down.
use once_cell::sync::Lazy;
use std::sync::atomic::AtomicU64;

#[derive(Debug)]
pub struct ExpressionConfig {
    /// Maximum number of VM instructions executed by one expression evaluation; 0 = no limit.
    /// Loops (`map`, `filter`, `some`, ...) execute a few instructions per element, so this bounds both the
    /// time and the memory a loop can take.
    pub max_steps: AtomicU64,
    /// Maximum number of elements an interval (`[a..b]`) may be expanded to when it is iterated; 0 = no limit.
    pub max_interval_length: AtomicU64,
}

impl Default for ExpressionConfig {
    fn default() -> Self {
        Self {
            max_steps: AtomicU64::new(0),
            max_interval_length: AtomicU64::new(0),
        }
    }
}

pub static EXPRESSION_CONFIG: Lazy<ExpressionConfig> = Lazy::new(Default::default);

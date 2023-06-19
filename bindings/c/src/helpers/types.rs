use async_trait::async_trait;
use std::ffi::c_void;
use std::sync::Arc;
use zen_engine::loader::{DecisionLoader, LoaderResponse};
use zen_engine::{Decision, DecisionEngine, EvaluationOptions};

pub type CZenDecision = Decision<DynDecisionLoader>;
pub type CZenDecisionEngine = DecisionEngine<DynDecisionLoader>;

pub type CZenDecisionPtr = c_void;
pub type CZenDecisionEnginePtr = c_void;

#[repr(C)]
pub struct CZenEngineEvaluationOptions {
    trace: bool,
    max_depth: u8,
}

impl Into<EvaluationOptions> for CZenEngineEvaluationOptions {
    fn into(self) -> EvaluationOptions {
        EvaluationOptions {
            trace: Some(self.trace),
            max_depth: Some(self.max_depth),
        }
    }
}

pub struct DynDecisionLoader {
    inner: Arc<dyn DecisionLoader + Send + Sync>,
}

impl DynDecisionLoader {
    pub fn new(inner: Arc<dyn DecisionLoader + Send + Sync>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl DecisionLoader for DynDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        self.inner.load(key).await
    }
}

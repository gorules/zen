use crate::loader::CDecisionLoader;
use async_trait::async_trait;
use std::ffi::c_void;
use zen_engine::loader::{DecisionLoader, LoaderResponse, NoopLoader};
use zen_engine::{Decision, DecisionEngine, EvaluationOptions};

pub(crate) type CZenDecision = Decision<DynamicDecisionLoader>;
pub(crate) type CZenDecisionEngine = DecisionEngine<DynamicDecisionLoader>;

pub type CZenDecisionPtr = c_void;
pub type CZenDecisionEnginePtr = c_void;

#[repr(C)]
#[derive(Debug)]
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

#[derive(Debug)]
pub(crate) enum DynamicDecisionLoader {
    Noop(NoopLoader),
    Native(CDecisionLoader),
    #[cfg(not(feature = "cdylib"))]
    Go(crate::languages::go::CGoDecisionLoader),
}

impl Default for DynamicDecisionLoader {
    fn default() -> Self {
        Self::Noop(Default::default())
    }
}

#[async_trait]
impl DecisionLoader for DynamicDecisionLoader {
    async fn load(&self, key: &str) -> LoaderResponse {
        match self {
            DynamicDecisionLoader::Noop(loader) => loader.load(key).await,
            DynamicDecisionLoader::Native(loader) => loader.load(key).await,
            #[cfg(not(feature = "cdylib"))]
            DynamicDecisionLoader::Go(loader) => loader.load(key).await,
        }
    }
}

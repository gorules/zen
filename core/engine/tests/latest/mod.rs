use zen_engine::policy::PolicyWorkspace;
use zen_engine::{Decision, DecisionEngine, EngineVersion};

pub trait Latest {
    fn latest(self) -> Self;
}

impl Latest for Decision {
    fn latest(self) -> Self {
        self.with_version(EngineVersion::V2)
    }
}

impl Latest for DecisionEngine {
    fn latest(self) -> Self {
        self.with_version(EngineVersion::V2)
    }
}

impl Latest for PolicyWorkspace {
    fn latest(self) -> Self {
        self.with_version(EngineVersion::V2)
    }
}

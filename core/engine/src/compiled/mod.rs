pub(crate) mod bound;
pub(crate) mod census;
pub(crate) mod content;
mod core;
pub(crate) mod decision;
pub mod diagnostics;
pub(crate) mod engine;
pub(crate) mod exact;
mod graph;
pub(crate) mod host;
mod leaf;
mod lowered;
mod output;
pub(crate) mod pinning;
pub(crate) mod policy;
pub(crate) mod prepared;
mod shred;
pub(crate) mod switches;
pub(crate) mod table;
pub(crate) mod trace;

pub use graph::ColumnarOutput;
pub(crate) use graph::{call, traced, CompiledGraph, CompiledPlan, Invalid};
pub use output::{OutputColumn, RecordsView};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineVersion {
    #[default]
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unknown engine version {0}, expected 1 or 2")]
pub struct UnknownEngineVersion(pub u32);

impl TryFrom<u32> for EngineVersion {
    type Error = UnknownEngineVersion;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(EngineVersion::V1),
            2 => Ok(EngineVersion::V2),
            other => Err(UnknownEngineVersion(other)),
        }
    }
}

impl From<EngineVersion> for u32 {
    fn from(value: EngineVersion) -> Self {
        match value {
            EngineVersion::V1 => 1,
            EngineVersion::V2 => 2,
        }
    }
}

impl From<EngineVersion> for Engine {
    fn from(value: EngineVersion) -> Self {
        match value {
            EngineVersion::V1 => Engine::Interpreted,
            EngineVersion::V2 => Engine::Compiled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    Interpreted,
    Compiled,
}

impl Engine {
    pub(crate) fn version(self) -> EngineVersion {
        match self {
            Engine::Compiled => EngineVersion::V2,
            Engine::Interpreted => EngineVersion::V1,
        }
    }

    pub(crate) fn compiled(self) -> bool {
        self == Engine::Compiled
    }
}

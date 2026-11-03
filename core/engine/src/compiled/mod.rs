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
pub(crate) mod policy;
pub(crate) mod prepared;
mod shred;
pub(crate) mod switches;
pub(crate) mod table;

pub use graph::ColumnarOutput;
pub(crate) use graph::{call, traced, CompiledGraph, CompiledPlan, Invalid};
pub use output::{OutputColumn, RecordsView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    Configured,
    Interpreted,
    Compiled,
}

impl Engine {
    pub(crate) fn compiled(self) -> bool {
        match self {
            Engine::Configured => crate::ZEN_CONFIG
                .compiled
                .load(std::sync::atomic::Ordering::Relaxed),
            Engine::Interpreted => false,
            Engine::Compiled => true,
        }
    }
}

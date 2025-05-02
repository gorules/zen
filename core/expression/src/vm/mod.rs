//! Virtual Machine - Evaluation of Opcodes
//!
//! The VM (Virtual Machine) module executes the generated machine-readable opcodes.
pub use error::VMError;
pub use vm::VM;

pub(crate) mod date;
mod error;
pub(crate) mod helpers;
mod interval;
mod vm;

pub(crate) use date::VmDate;

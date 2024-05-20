//! Virtual Machine - Evaluation of Opcodes
//!
//! The VM (Virtual Machine) module executes the generated machine-readable opcodes.
pub use error::VMError;
pub use vm::VM;

mod error;
pub(crate) mod helpers;
mod variable;
mod vm;

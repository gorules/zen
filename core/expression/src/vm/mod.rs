//! Virtual Machine - Evaluation of Opcodes
//!
//! The VM (Virtual Machine) module executes the generated machine-readable opcodes.
mod error;
pub(crate) mod helpers;
mod variable;
mod vm;

pub use error::VMError;
pub use variable::Variable;
pub use vm::VM;

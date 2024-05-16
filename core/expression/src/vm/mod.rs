//! Virtual Machine - Evaluation of Opcodes
//!
//! The VM (Virtual Machine) module executes the generated machine-readable opcodes.
pub use error::VMError;
pub use variable::Variable;
pub(crate) use vm::NULL_VAR;
pub use vm::VM;

mod error;
pub(crate) mod helpers;
mod variable;
mod vm;

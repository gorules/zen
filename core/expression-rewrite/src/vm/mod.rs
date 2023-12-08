//! Evaluation of Opcodes
mod error;
mod helpers;
mod variable;
mod vm;

pub use error::VMError;
pub use variable::Variable;
pub use vm::VM;

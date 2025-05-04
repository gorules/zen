//! Compilation from AST into Opcodes
//!
//! The Compiler module transforms an Abstract Syntax Tree (AST) representation of source code into machine-readable opcodes.
mod compiler;
mod error;
mod opcode;

pub use compiler::Compiler;
pub use error::CompilerError;
pub use opcode::{Compare, FetchFastTarget, Jump, Opcode};

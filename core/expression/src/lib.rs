#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

extern crate core;

mod helpers;

pub mod ast;
pub mod compiler;
pub mod isolate;
pub mod lexer;
pub mod opcodes;
pub mod parser;
pub mod vm;

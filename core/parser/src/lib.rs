#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![allow(clippy::module_inception)]

pub mod ast;
pub mod lexer;
pub mod parser;

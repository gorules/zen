#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

extern crate core;

mod helpers;

pub mod compiler;
pub mod isolate;
pub mod opcodes;
pub mod vm;

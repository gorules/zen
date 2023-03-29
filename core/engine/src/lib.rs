#![deny(clippy::unwrap_used)]
#![allow(clippy::module_inception)]

mod handler;

pub mod decision;
pub mod engine;
pub mod loader;
pub mod model;

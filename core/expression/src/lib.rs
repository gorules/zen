//! A lightweight expression language designed for the evaluation of expressions in various contexts.
//!
//! Zen Expression is a versatile single-threaded expression language designed for simplicity
//! high-performance. It's primarily used for evaluating and processing JSON data offers key components that empower developers in creating responsive and
//! non-blocking I/O applications
//! Out of the box, it comes with amazing benefits:
//! - 🚀 Blazingly fast - Perform millions of evaluations per second
//! - 🧠 Intuitive syntax - Minimalistic and expressive syntax
//! - 💼 Portable - Can be compiled for all standard architectures including WASM
//!
//! For a full list of language references, visit [documentation](https://gorules.io/docs/rules-engine/expression-language/).
//!
//! # Example
//! Evaluate expression using isolate:
//! ```
//! use zen_expression::{evaluate_expression, json};
//!
//! fn main() {
//!     let context = json!({ "tax": { "percentage": 10 } });
//!     let tax_amount = evaluate_expression("50 * tax.percentage / 100", &context).unwrap();
//!
//!     assert_eq!(tax_amount, json!(5));
//! }
//! ```
//!
//! ## High Performance
//! When evaluating a lot of expressions at once, you can use Isolate directly. Under the hood, Isolate
//! will re-use allocated memory from previous evaluations, drastically improving performance.
//!
//! ```
//! use zen_expression::{Isolate, json};
//!
//! fn main() {
//!     let context = json!({ "tax": { "percentage": 10 } });
//!     let mut isolate = Isolate::with_environment(&context);
//!
//!     // Fast 🚀
//!     for _ in 0..1_000 {
//!         let tax_amount = isolate.run_standard("50 * tax.percentage / 100").unwrap();
//!         assert_eq!(tax_amount, json!(5));
//!     }
//! }
//! ```
//!
//! # Feature flags
//!
//! Name | Description | Default?
//! ---|---|---
//! `regex-deprecated` | Uses standard `regex` crate | Yes
//! `regex-lite` | Opts for usage of lightweight `regex-lite` crate. Useful for reducing build size, especially in WASM. | No

mod isolate;

pub mod compiler;
mod function;
pub mod lexer;
pub mod parser;
pub mod vm;

pub use function::{evaluate_expression, evaluate_unary_expression};
pub use isolate::{Isolate, IsolateError};
pub use serde_json::json;

//! Parses Tokens into AST
//!
//! The Parser module processes tokens from the Lexer, constructing an Abstract Syntax Tree (AST).
//!
//! It's available in two specialized variants:
//! - Standard, designed for comprehensive expression evaluation yielding any result
//! - Unary, specifically created for truthy tests with exclusive boolean outcomes
mod ast;
mod builtin;
mod constants;
mod error;
mod parser;
mod sanitised_string;
mod standard;
mod unary;

pub use ast::Node;
pub use builtin::BuiltInFunction;
pub use error::ParserError;
pub use parser::Parser;
pub use standard::Standard;
pub use unary::Unary;

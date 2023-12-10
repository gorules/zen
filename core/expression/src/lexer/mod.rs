//! Performs lexical analysis on string inputs
//!
//! The Lexer module transforms strings into tokens using Strum.
mod error;
mod token;

mod codes;
mod cursor;
mod lexer;

pub use error::LexerError;
pub use lexer::Lexer;
pub use token::*;

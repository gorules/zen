use thiserror::Error;
use crate::parser::ast::AstNodeError;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum ParserError {
    #[error("Unexpected token: received {received} instead of {expected} at ({}, {})", span.0, span.1)]
    UnexpectedToken {
        expected: String,
        received: String,
        span: (u32, u32),
    },

    #[error("Failed to parse: {message} at ({}, {})", span.0, span.1)]
    FailedToParse { message: String, span: (u32, u32) },

    #[error("Unknown built in: {name} at ({}, {})", span.0, span.1)]
    UnknownBuiltIn { name: String, span: (u32, u32) },
    
    #[error("Token out of bounds")]
    TokenOutOfBounds,

    #[error("Memory failure")]
    MemoryFailure,
}

pub(crate) type ParserResult<T> = Result<T, ParserError>;
pub(crate) type AstResult<T> = Result<T, AstNodeError>;

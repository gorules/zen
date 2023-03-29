use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Error)]
pub enum ParserError {
    #[error("Token out of bounds")]
    TokenOutOfBounds,

    #[error("Memory failure")]
    MemoryFailure,

    #[error("Unexpected token: received {received} instead of {expected}")]
    UnexpectedToken { expected: String, received: String },

    #[error("Failed to parse: {message}")]
    FailedToParse { message: String },

    #[error("Unknown built in: {token}")]
    UnknownBuiltIn { token: String },

    #[error("Unsupported built in: {token}")]
    UnsupportedBuiltIn { token: String },
}

pub type ParserResult<T> = Result<T, ParserError>;

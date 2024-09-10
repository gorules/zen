use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum LexerError {
    #[error("Unexpected symbol: {symbol} at ({}, {})", span.0, span.1)]
    UnexpectedSymbol { symbol: String, span: (u32, u32) },

    #[error("Unmatched symbol: {symbol} at {position}")]
    UnmatchedSymbol { symbol: char, position: u32 },

    #[error("Unexpected EOF: {symbol} at {position}")]
    UnexpectedEof { symbol: char, position: u32 },
}

pub(crate) type LexerResult<T> = Result<T, LexerError>;

use strum::ParseError;
use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum LexerError {
    #[error("Unexpected symbol: {symbol}")]
    UnexpectedSymbol { symbol: String },

    #[error("Unmatched symbol: {symbol} at position {position}")]
    UnmatchedSymbol { symbol: char, position: usize },

    #[error("Unexpected end of file: {symbol} at position {position}")]
    UnexpectedEof { symbol: char, position: usize },
}

impl From<ParseError> for LexerError {
    fn from(value: ParseError) -> Self {
        Self::UnexpectedSymbol {
            symbol: value.to_string(),
        }
    }
}

pub(crate) type LexerResult<T> = Result<T, LexerError>;

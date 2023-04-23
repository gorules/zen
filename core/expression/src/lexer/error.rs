use thiserror::Error;

#[derive(Debug, Error)]
pub enum LexerError {
    #[error("Unexpected symbol: {symbol}")]
    UnexpectedSymbol { symbol: String },

    #[error("Unmatched symbol: {symbol} at position {position}")]
    UnmatchedSymbol { symbol: char, position: usize },

    #[error("Unexpected end of file: {symbol} at position {position}")]
    UnexpectedEof { symbol: char, position: usize },
}

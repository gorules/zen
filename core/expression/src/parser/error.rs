use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum ParserError {
    #[error("{0}")]
    NodeError(String),

    #[error("Incomplete parser output")]
    Incomplete,
}

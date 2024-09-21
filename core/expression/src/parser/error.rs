use crate::parser::ast::AstNodeError;
use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum ParserError {
    #[error("{0}")]
    NodeError(AstNodeError),

    #[error("Incomplete parser output")]
    Incomplete,
}

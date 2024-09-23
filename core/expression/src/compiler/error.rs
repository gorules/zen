use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum CompilerError {
    #[error("Unknown unary operator: {operator}")]
    UnknownUnaryOperator { operator: String },

    #[error("Unknown binary operator: {operator}")]
    UnknownBinaryOperator { operator: String },

    #[error("Argument not found for builtin {builtin} at index {index}")]
    ArgumentNotFound { builtin: String, index: usize },

    #[error("Unexpected error node")]
    UnexpectedErrorNode,
}

pub(crate) type CompilerResult<T> = Result<T, CompilerError>;

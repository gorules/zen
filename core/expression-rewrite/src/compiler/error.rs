use thiserror::Error;

#[derive(Debug, Error)]
pub enum CompilerError {
    #[error("Unknown unary operator: {operator}")]
    UnknownUnaryOperator { operator: String },

    #[error("Unknown binary operator: {operator}")]
    UnknownBinaryOperator { operator: String },

    #[error("Argument not found for builtin {builtin} at index {index}")]
    ArgumentNotFound { builtin: String, index: usize },
}

pub(crate) type CompilerResult<T> = Result<T, CompilerError>;
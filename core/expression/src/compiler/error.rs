use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum CompilerError {
    #[error("Unknown unary operator: {operator}")]
    UnknownUnaryOperator { operator: String },

    #[error("Unknown binary operator: {operator}")]
    UnknownBinaryOperator { operator: String },

    #[error("Argument not found for function {function} at index {index}")]
    ArgumentNotFound { function: String, index: usize },

    #[error("Unexpected error node")]
    UnexpectedErrorNode,

    #[error("Unknown function `{name}`")]
    UnknownFunction { name: String },

    #[error("Invalid function call `{name}`: {message}")]
    InvalidFunctionCall { name: String, message: String },

    #[error("Invalid  method call `{name}`: {message}")]
    InvalidMethodCall { name: String, message: String },
}

pub(crate) type CompilerResult<T> = Result<T, CompilerError>;

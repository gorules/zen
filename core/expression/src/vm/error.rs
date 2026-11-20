use thiserror::Error;

#[derive(Debug, PartialEq, Eq, Clone, Error)]
pub enum VMError {
    #[error("Opcode {opcode}: {message}")]
    OpcodeErr { opcode: String, message: String },

    #[error("Opcode out of bounds")]
    OpcodeOutOfBounds { index: usize, bytecode: String },

    #[error("Stack out of bounds")]
    StackOutOfBounds { stack: String },

    #[error("Failed to parse date time")]
    ParseDateTimeErr { timestamp: String },

    #[error("Number conversion error")]
    NumberConversionError,

    #[error("Evaluation budget exceeded: {budget} over {limit}")]
    BudgetExceeded { budget: &'static str, limit: u64 },
}

pub(crate) type VMResult<T> = Result<T, VMError>;

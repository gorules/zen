use std::fmt::{Display, Formatter};

use rquickjs::{CaughtError, Error};

pub type FunctionResult<Ok = ()> = Result<Ok, FunctionError>;

#[derive(Debug)]
pub enum FunctionError {
    Caught(String),
    Runtime(Error),
}

impl<'js> From<CaughtError<'js>> for FunctionError {
    fn from(value: CaughtError<'js>) -> Self {
        Self::Caught(value.to_string())
    }
}

impl From<Error> for FunctionError {
    fn from(value: Error) -> Self {
        Self::Runtime(value)
    }
}

impl Display for FunctionError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FunctionError::Caught(c) => f.write_str(c.as_str()),
            FunctionError::Runtime(rt) => rt.fmt(f),
        }
    }
}

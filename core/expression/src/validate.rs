use crate::{Isolate, IsolateError};

pub fn validate_unary_expression(expression: &str) -> Result<(), IsolateError> {
    let mut isolate = Isolate::new();
    isolate.compile_unary(expression)?;

    Ok(())
}

pub fn validate_expression(expression: &str) -> Result<(), IsolateError> {
    let mut isolate = Isolate::new();
    isolate.compile_standard(expression)?;

    Ok(())
}

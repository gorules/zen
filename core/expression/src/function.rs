use serde_json::Value;

use crate::{Isolate, IsolateError};

/// Evaluates a standard expression
pub fn evaluate_expression(expression: &str, context: &Value) -> Result<Value, IsolateError> {
    Isolate::with_environment(context).run_standard(expression)
}

/// Evaluates a unary expression; Required: context must be an object with "$" key.
pub fn evaluate_unary_expression(expression: &str, context: &Value) -> Result<bool, IsolateError> {
    let Some(context_object) = context.as_object() else {
        return Err(IsolateError::MissingContextReference);
    };

    if !context_object.contains_key("$") {
        return Err(IsolateError::MissingContextReference);
    }

    Isolate::with_environment(context).run_unary(expression)
}

#[cfg(test)]
mod test {
    use crate::evaluate_expression;
    use serde_json::json;

    #[test]
    fn bla() {
        let context = json!({ "tax": { "percentage": 10 } });
        let tax_amount = evaluate_expression("50 * tax.percentage / 100", &context).unwrap();

        assert_eq!(tax_amount, json!(5));
    }
}

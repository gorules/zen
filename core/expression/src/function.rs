use crate::variable::Variable;
use crate::{Isolate, IsolateError};

/// Evaluates a standard expression
pub fn evaluate_expression(expression: &str, context: Variable) -> Result<Variable, IsolateError> {
    Isolate::with_environment(context).run_standard(expression)
}

/// Evaluates a unary expression; Required: context must be an object with "$" key.
pub fn evaluate_unary_expression(
    expression: &str,
    context: Variable,
) -> Result<bool, IsolateError> {
    let Some(context_object_ref) = context.as_object() else {
        return Err(IsolateError::MissingContextReference);
    };

    let context_object = context_object_ref.borrow();
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
        let tax_amount = evaluate_expression("50 * tax.percentage / 100", context.into()).unwrap();

        assert_eq!(tax_amount, json!(5).into());
    }
}

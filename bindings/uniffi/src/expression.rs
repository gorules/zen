use crate::error::ZenError;
use crate::types::JsonBuffer;
use zen_expression::expression::{Standard, Unary};
use zen_expression::{Expression, Variable};

#[uniffi::export]
pub fn evaluate_expression(
    expression: String,
    context: Option<JsonBuffer>,
) -> Result<JsonBuffer, ZenError> {
    let ctx: Variable = context
        .map(Variable::try_from)
        .transpose()?
        .unwrap_or(Variable::Null);

    zen_expression::evaluate_expression(expression.as_str(), ctx).map(JsonBuffer::try_from)?
}

#[allow(dead_code)]
#[uniffi::export]
pub fn evaluate_unary_expression(
    expression: String,
    context: JsonBuffer,
) -> Result<bool, ZenError> {
    let ctx: Variable = context.try_into()?;

    Ok(zen_expression::evaluate_unary_expression(
        expression.as_str(),
        ctx,
    )?)
}

#[derive(uniffi::Object)]
pub(crate) struct ZenExpression {
    expression: Expression<Standard>,
}

#[uniffi::export]
impl ZenExpression {
    #[uniffi::constructor]
    pub fn compile(expression: String) -> Result<Self, ZenError> {
        zen_expression::compile_expression(expression.as_str())
            .map_err(|err| ZenError::IsolateError(err.to_string()))
            .map(|expression| Self { expression })
    }

    pub fn evaluate(&self, context: Option<JsonBuffer>) -> Result<JsonBuffer, ZenError> {
        let ctx: Variable = context
            .map(Variable::try_from)
            .transpose()?
            .unwrap_or(Variable::Null);

        self.expression.evaluate(ctx).map(JsonBuffer::try_from)?
    }
}

#[derive(uniffi::Object)]
pub(crate) struct ZenExpressionUnary {
    expression: Expression<Unary>,
}

#[uniffi::export]
impl ZenExpressionUnary {
    #[uniffi::constructor]
    pub fn compile(expression: String) -> Result<Self, ZenError> {
        zen_expression::compile_unary_expression(expression.as_str())
            .map_err(|err| ZenError::IsolateError(err.to_string()))
            .map(|expression| Self { expression })
    }

    pub fn evaluate(&self, context: JsonBuffer) -> Result<bool, ZenError> {
        let ctx: Variable = context.try_into()?;
        Ok(self.expression.evaluate(ctx)?)
    }
}

use crate::error::ZenError;
use crate::types::JsonBuffer;
use std::sync::Arc;
use zen_expression::expression::{Standard, Unary};
use zen_expression::{Expression, Variable};

#[uniffi::export]
pub fn evaluate_expression(
    expression: String,
    context: Option<Arc<JsonBuffer>>,
) -> Result<JsonBuffer, ZenError> {
    let ctx: Variable = context.map(|v| v.to_variable()).unwrap_or(Variable::Null);

    Ok(
        zen_expression::evaluate_expression(expression.as_str(), ctx)
            .map_err(|e| {
                ZenError::EvaluationError(
                    serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()),
                )
            })?
            .into(),
    )
}

#[allow(dead_code)]
#[uniffi::export]
pub fn evaluate_unary_expression(
    expression: String,
    context: Arc<JsonBuffer>,
) -> Result<bool, ZenError> {
    let ctx: Variable = context.to_variable();

    Ok(
        zen_expression::evaluate_unary_expression(expression.as_str(), ctx).map_err(|e| {
            ZenError::EvaluationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        })?,
    )
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

    pub fn evaluate(&self, context: Option<Arc<JsonBuffer>>) -> Result<JsonBuffer, ZenError> {
        let ctx: Variable = context.map(|b| b.to_variable()).unwrap_or(Variable::Null);
        let res = self.expression.evaluate(ctx).map_err(|e| {
            ZenError::EvaluationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        });

        Ok(res?.into())
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

    pub fn evaluate(&self, context: Arc<JsonBuffer>) -> Result<bool, ZenError> {
        let ctx: Variable = context.to_variable();

        self.expression.evaluate(ctx).map_err(|e| {
            ZenError::EvaluationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        })
    }
}

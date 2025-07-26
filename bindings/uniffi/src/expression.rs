use crate::error::ZenError;
use crate::types::JsonBuffer;
use tokio::task;
use zen_expression::expression::{Standard, Unary};
use zen_expression::{Expression, Variable};

#[uniffi::export()]
pub fn evaluate_expression_sync(
    expression: String,
    context: Option<JsonBuffer>,
) -> Result<JsonBuffer, ZenError> {
    let ctx: Variable = context
        .and_then(|v| v.try_into().ok())
        .unwrap_or(Variable::Null);

    Ok(
        zen_expression::evaluate_expression(expression.as_str(), ctx)
            .map_err(|e| {
                ZenError::EvaluationError(
                    serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()),
                )
            })?
            .try_into()?,
    )
}

#[allow(dead_code)]
#[uniffi::export()]
pub fn evaluate_unary_expression_sync(
    expression: String,
    context: JsonBuffer,
) -> Result<bool, ZenError> {
    let ctx: Variable = context.try_into()?;

    Ok(
        zen_expression::evaluate_unary_expression(expression.as_str(), ctx).map_err(|e| {
            ZenError::EvaluationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        })?,
    )
}

#[allow(dead_code)]
#[uniffi::export(async_runtime = "tokio")]
pub async fn evaluate_expression(
    expression: String,
    context: Option<JsonBuffer>,
) -> Result<JsonBuffer, ZenError> {
    task::spawn_blocking(move || evaluate_expression_sync(expression, context))
        .await
        .map_err(|_| ZenError::ExecutionTaskSpawnError)?
}

#[allow(dead_code)]
#[uniffi::export(async_runtime = "tokio")]
pub async fn evaluate_unary_expression(
    expression: String,
    context: JsonBuffer,
) -> Result<bool, ZenError> {
    task::spawn_blocking(move || evaluate_unary_expression_sync(expression, context))
        .await
        .map_err(|_| ZenError::ExecutionTaskSpawnError)?
}

#[allow(dead_code)]
#[uniffi::export()]
pub fn compile_expression(expression: String) -> Result<ZenExpression, ZenError> {
    zen_expression::compile_expression(expression.as_str())
        .map_err(|err| ZenError::IsolateError(err.to_string()))
        .map(|expression| ZenExpression { expression })
}

#[allow(dead_code)]
#[uniffi::export()]
pub fn compile_unary_expression(expression: String) -> Result<ZenExpressionUnary, ZenError> {
    zen_expression::compile_unary_expression(expression.as_str())
        .map_err(|err| ZenError::IsolateError(err.to_string()))
        .map(|expression| ZenExpressionUnary { expression })
}

#[derive(uniffi::Object)]
pub(crate) struct ZenExpression {
    expression: Expression<Standard>,
}

#[uniffi::export(async_runtime = "tokio")]
impl ZenExpression {
    pub async fn evaluate(&self, context: JsonBuffer) -> Result<JsonBuffer, ZenError> {
        let expression = ZenExpression {
            expression: self.expression.clone(),
        };

        task::spawn_blocking(move || expression.evaluate_sync(context))
            .await
            .map_err(|_| ZenError::ExecutionTaskSpawnError)?
    }

    pub fn evaluate_sync(&self, context: JsonBuffer) -> Result<JsonBuffer, ZenError> {
        let ctx: Variable = context.try_into()?;
        let res = self.expression.evaluate(ctx).map_err(|e| {
            ZenError::EvaluationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        });

        res?.try_into()
    }
}

#[derive(uniffi::Object)]
pub(crate) struct ZenExpressionUnary {
    expression: Expression<Unary>,
}

#[uniffi::export(async_runtime = "tokio")]
impl ZenExpressionUnary {
    pub async fn evaluate(&self, context: JsonBuffer) -> Result<bool, ZenError> {
        let expression = ZenExpressionUnary {
            expression: self.expression.clone(),
        };

        task::spawn_blocking(move || expression.evaluate_sync(context))
            .await
            .map_err(|_| ZenError::ExecutionTaskSpawnError)?
    }

    pub fn evaluate_sync(&self, context: JsonBuffer) -> Result<bool, ZenError> {
        let ctx: Variable = context.try_into()?;

        self.expression.evaluate(ctx).map_err(|e| {
            ZenError::EvaluationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        })
    }
}

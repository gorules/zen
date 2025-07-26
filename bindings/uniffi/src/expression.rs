use crate::error::ZenError;
use crate::types::JsonBuffer;
use std::sync::Arc;
use tokio::task;
use zen_expression::expression::{Standard, Unary};
use zen_expression::{Expression, Variable};

#[uniffi::export()]
pub fn evaluate_expression_sync(
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
#[uniffi::export()]
pub fn evaluate_unary_expression_sync(
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

#[allow(dead_code)]
#[uniffi::export(async_runtime = "tokio")]
pub async fn evaluate_expression(
    expression: String,
    context: Option<Arc<JsonBuffer>>,
) -> Result<JsonBuffer, ZenError> {
    task::spawn_blocking(move || evaluate_expression_sync(expression, context))
        .await
        .map_err(|_| ZenError::ExecutionTaskSpawnError)?
}

#[allow(dead_code)]
#[uniffi::export(async_runtime = "tokio")]
pub async fn evaluate_unary_expression(
    expression: String,
    context: Arc<JsonBuffer>,
) -> Result<bool, ZenError> {
    task::spawn_blocking(move || evaluate_unary_expression_sync(expression, context))
        .await
        .map_err(|_| ZenError::ExecutionTaskSpawnError)?
}

#[derive(uniffi::Object)]
pub(crate) struct ZenExpression {
    expression: Expression<Standard>,
}

#[uniffi::export(async_runtime = "tokio")]
impl ZenExpression {
    #[uniffi::constructor]
    pub fn compile(expression: String) -> Result<Self, ZenError> {
        zen_expression::compile_expression(expression.as_str())
            .map_err(|err| ZenError::IsolateError(err.to_string()))
            .map(|expression| Self { expression })
    }

    pub async fn evaluate(&self, context: Option<Arc<JsonBuffer>>) -> Result<JsonBuffer, ZenError> {
        let expression = Self {
            expression: self.expression.clone(),
        };

        task::spawn_blocking(move || expression.evaluate_sync(context))
            .await
            .map_err(|_| ZenError::ExecutionTaskSpawnError)?
    }

    pub fn evaluate_sync(&self, context: Option<Arc<JsonBuffer>>) -> Result<JsonBuffer, ZenError> {
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

#[uniffi::export(async_runtime = "tokio")]
impl ZenExpressionUnary {
    #[uniffi::constructor]
    pub fn compile(expression: String) -> Result<Self, ZenError> {
        zen_expression::compile_unary_expression(expression.as_str())
            .map_err(|err| ZenError::IsolateError(err.to_string()))
            .map(|expression| Self { expression })
    }

    pub async fn evaluate(&self, context: Arc<JsonBuffer>) -> Result<bool, ZenError> {
        let expression = Self {
            expression: self.expression.clone(),
        };

        task::spawn_blocking(move || expression.evaluate_sync(context))
            .await
            .map_err(|_| ZenError::ExecutionTaskSpawnError)?
    }

    pub fn evaluate_sync(&self, context: Arc<JsonBuffer>) -> Result<bool, ZenError> {
        let ctx: Variable = context.to_variable();

        self.expression.evaluate(ctx).map_err(|e| {
            ZenError::EvaluationError(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()))
        })
    }
}

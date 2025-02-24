use crate::error::ZenError;
use crate::types::JsonBuffer;
use tokio::task;
use zen_expression::Variable;

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
#[uniffi::export()]
pub fn render_template_sync(template: String, context: JsonBuffer) -> Result<JsonBuffer, ZenError> {
    let ctx: Variable = context.try_into()?;

    Ok(zen_tmpl::render(template.as_str(), ctx)
        .map_err(|e| ZenError::TemplateEngineError {
            template,
            details: serde_json::to_string(&e).unwrap_or_else(|_| e.to_string()),
        })?
        .to_value()
        .try_into()?)
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
#[uniffi::export(async_runtime = "tokio")]
pub async fn render_template(
    template: String,
    context: JsonBuffer,
) -> Result<JsonBuffer, ZenError> {
    task::spawn_blocking(move || render_template_sync(template, context))
        .await
        .map_err(|_| ZenError::ExecutionTaskSpawnError)?
}

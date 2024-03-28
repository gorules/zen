use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::Value;

#[allow(dead_code)]
#[napi]
pub async fn evaluate_expression(
    expression: String,
    context: Option<Value>,
) -> napi::Result<Value> {
    let ctx = context.unwrap_or(Value::Null);

    let result: Value = napi::tokio::spawn(async move {
        zen_expression::evaluate_expression(expression.as_str(), &ctx)
    })
    .await
    .map_err(|_| anyhow!("Hook timed out"))?
    .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

#[allow(dead_code)]
#[napi]
pub async fn evaluate_unary_expression(expression: String, context: Value) -> napi::Result<bool> {
    let result: bool = napi::tokio::spawn(async move {
        zen_expression::evaluate_unary_expression(expression.as_str(), &context)
    })
    .await
    .map_err(|_| anyhow!("Hook timed out"))?
    .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

#[allow(dead_code)]
#[napi]
pub async fn render_template(template: String, context: Value) -> napi::Result<Value> {
    let result: Value =
        napi::tokio::spawn(async move { zen_template::render(template.as_str(), &context) })
            .await
            .map_err(|_| anyhow!("Hook timed out"))?
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

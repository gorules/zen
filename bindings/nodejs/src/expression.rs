use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::{json, Value};

#[napi]
pub fn evaluate_expression_sync(expression: String, context: Option<Value>) -> napi::Result<Value> {
    let ctx = context.unwrap_or(Value::Null);

    Ok(
        zen_expression::evaluate_expression(expression.as_str(), ctx.into())
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?
            .to_value(),
    )
}

#[napi]
pub fn list_expression_identifiers_sync(expression: String) -> napi::Result<Value> {
    let identifiers = zen_expression::extract_expression_identifiers(expression.as_str());
    Ok(json!(identifiers))
}

#[allow(dead_code)]
#[napi]
pub fn evaluate_unary_expression_sync(expression: String, context: Value) -> napi::Result<bool> {
    Ok(
        zen_expression::evaluate_unary_expression(expression.as_str(), context.into())
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?,
    )
}

#[allow(dead_code)]
#[napi]
pub fn render_template_sync(template: String, context: Value) -> napi::Result<Value> {
    Ok(zen_tmpl::render(template.as_str(), context.into())
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?
        .to_value())
}

#[allow(dead_code)]
#[napi]
pub async fn evaluate_expression(
    expression: String,
    context: Option<Value>,
) -> napi::Result<Value> {
    napi::tokio::spawn(async move { evaluate_expression_sync(expression, context) })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
}

#[allow(dead_code)]
#[napi]
pub async fn evaluate_unary_expression(expression: String, context: Value) -> napi::Result<bool> {
    napi::tokio::spawn(async move { evaluate_unary_expression_sync(expression, context) })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
}

#[allow(dead_code)]
#[napi]
pub async fn render_template(template: String, context: Value) -> napi::Result<Value> {
    napi::tokio::spawn(async move { render_template_sync(template, context) })
        .await
        .map_err(|_| anyhow!("Hook timed out"))?
}

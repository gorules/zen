use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::Value;

use zen_expression::isolate::Isolate;

#[allow(dead_code)]
#[napi]
pub async fn evaluate_expression(
    expression: String,
    context: Option<Value>,
) -> napi::Result<Value> {
    let result: Value = napi::tokio::spawn(async move {
        let isolate = Isolate::default();
        if let Some(ctx) = context {
            isolate.inject_env(&ctx);
        }

        isolate.run_standard(expression.as_str())
    })
    .await
    .map_err(|_| anyhow!("Hook timed out"))?
    .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

#[allow(dead_code)]
#[napi]
pub async fn evaluate_unary_expression(expression: String, context: Value) -> napi::Result<Value> {
    let Some(context_object) = context.as_object() else {
        return Err(anyhow!("Context must be an object").into());
    };

    if !context_object.contains_key("$") {
        return Err(anyhow!("Context must contain '$' reference.").into());
    }

    let result: Value = napi::tokio::spawn(async move {
        let isolate = Isolate::default();
        isolate.inject_env(&context);

        isolate.run_unary(expression.as_str())
    })
    .await
    .map_err(|_| anyhow!("Hook timed out"))?
    .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

    Ok(result)
}

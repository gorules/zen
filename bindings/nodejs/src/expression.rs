use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::Value;

#[napi]
pub fn evaluate_expression_sync(expression: String, context: Option<Value>) -> napi::Result<Value> {
    let ctx = zen_expression::Variable::try_from_value(context.unwrap_or(Value::Null))
        .map_err(|e| anyhow!(e))?;

    Ok(
        zen_expression::evaluate_expression(expression.as_str(), ctx)
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?
            .to_value(),
    )
}

#[allow(dead_code)]
#[napi]
pub fn evaluate_unary_expression_sync(expression: String, context: Value) -> napi::Result<bool> {
    let context = zen_expression::Variable::try_from_value(context).map_err(|e| anyhow!(e))?;
    Ok(
        zen_expression::evaluate_unary_expression(expression.as_str(), context)
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?,
    )
}

#[allow(dead_code)]
#[napi]
pub fn render_template_sync(template: String, context: Value) -> napi::Result<Value> {
    let context = zen_expression::Variable::try_from_value(context).map_err(|e| anyhow!(e))?;
    Ok(zen_tmpl::render(template.as_str(), context)
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

pub(crate) fn json_to_variable_type(value: &Value) -> zen_expression::variable::VariableType {
    use std::rc::Rc;
    use zen_expression::variable::VariableType as VT;

    let Some(tag) = value.get("type").and_then(Value::as_str) else {
        return VT::Any;
    };

    match tag {
        "any" => VT::Any,
        "null" => VT::Null,
        "bool" => VT::Bool,
        "string" => VT::String,
        "number" => VT::Number,
        "date" => VT::Date,
        "interval" => VT::Interval,
        "const" => value
            .get("value")
            .and_then(Value::as_str)
            .map(|s| VT::Const(Rc::from(s)))
            .unwrap_or(VT::Any),
        "enum" => {
            let name = value.get("name").and_then(Value::as_str).map(Rc::from);
            let values = value
                .get("values")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(Value::as_str).map(Rc::from).collect())
                .unwrap_or_default();
            VT::Enum(name, values)
        }
        "array" => {
            let items = value
                .get("items")
                .map(json_to_variable_type)
                .unwrap_or(VT::Any);
            VT::Array(Rc::new(items))
        }
        "object" => {
            let object = VT::empty_object();
            if let (VT::Object(map), Some(fields)) =
                (&object, value.get("fields").and_then(Value::as_object))
            {
                for (key, field) in fields {
                    map.borrow_mut()
                        .insert(Rc::from(key.as_str()), json_to_variable_type(field));
                }
            }
            object
        }
        "nullable" => {
            let inner = value
                .get("inner")
                .map(json_to_variable_type)
                .unwrap_or(VT::Any);
            VT::Nullable(Rc::new(inner))
        }
        _ => VT::Any,
    }
}

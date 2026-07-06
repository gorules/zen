use napi::anyhow::anyhow;
use napi_derive::napi;
use serde_json::Value;

#[napi]
pub fn evaluate_expression_sync(expression: String, context: Option<Value>) -> napi::Result<Value> {
    let ctx = context.unwrap_or(Value::Null);

    Ok(
        zen_expression::evaluate_expression(expression.as_str(), ctx.into())
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?
            .to_value(),
    )
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

#[napi]
pub fn nl_encode_string(value: String) -> Option<String> {
    zen_expression::nl::encode_string(&value)
}

#[napi(object)]
pub struct NlTokenizeRequest {
    pub id: String,
    pub expression: String,
    pub unary: bool,
    #[napi(ts_type = "PolicyVariableType")]
    pub subject_type: Option<Value>,
}

#[napi(
    ts_args_type = "requests: NlTokenizeRequest[], rootType: PolicyVariableType, strict?: boolean",
    ts_return_type = "NlResult[]"
)]
pub fn nl_tokenize_batch(
    requests: Vec<NlTokenizeRequest>,
    root_type: Value,
    strict: Option<bool>,
) -> napi::Result<Vec<Value>> {
    use zen_expression::intellisense::IntelliSense;
    use zen_expression::nl::NlRequest;

    let root = json_to_variable_type(&root_type);
    let core_requests: Vec<NlRequest> = requests
        .into_iter()
        .map(|request| NlRequest {
            id: request.id,
            expression: request.expression,
            unary: request.unary,
            subject_type: request.subject_type.as_ref().map(json_to_variable_type),
        })
        .collect();

    let mut intellisense = IntelliSense::new().with_strict(strict.unwrap_or(false));
    intellisense
        .nl_tokenize_batch(&core_requests, &root)
        .iter()
        .map(|result| {
            let mut value =
                serde_json::to_value(result).map_err(|e| napi::Error::from_reason(e.to_string()))?;
            if let (Some(subject), Some(obj)) = (&result.subject_type, value.as_object_mut()) {
                obj.insert(
                    "subjectType".into(),
                    crate::policy::variable_type_to_json(subject),
                );
            }
            Ok(value)
        })
        .collect()
}

fn json_to_variable_type(value: &Value) -> zen_expression::variable::VariableType {
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

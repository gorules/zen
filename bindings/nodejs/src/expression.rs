use napi::anyhow::anyhow;
use napi::bindgen_prelude::{Buffer, Uint32Array};
use napi_derive::napi;
use serde_json::Value;

use crate::mt::spawn_worker;

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

struct ProjectedContext<'a> {
    roots: &'a std::collections::HashSet<String>,
}

impl<'de> serde::de::Visitor<'de> for ProjectedContext<'_> {
    type Value = zen_expression::Variable;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        let mut object = zen_expression::variable::VariableMap::new();
        while let Some(key) = access.next_key::<std::borrow::Cow<'de, str>>()? {
            if self.roots.contains(key.as_ref()) {
                object.insert(
                    key.as_ref().into(),
                    access.next_value::<zen_expression::Variable>()?,
                );
            } else {
                access.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(zen_expression::Variable::from_object(object))
    }
}

impl<'de> serde::de::DeserializeSeed<'de> for ProjectedContext<'_> {
    type Value = zen_expression::Variable;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(self)
    }
}

fn parse_context(
    payload: &[u8],
    roots: Option<&std::collections::HashSet<String>>,
) -> Option<zen_expression::Variable> {
    if let Some(roots) = roots {
        let mut deserializer = serde_json::Deserializer::from_slice(payload);
        let seed = ProjectedContext { roots };
        if let Ok(context) = serde::de::DeserializeSeed::deserialize(seed, &mut deserializer) {
            return Some(context);
        }
    }
    serde_json::from_slice(payload).ok()
}

fn matching_indices(expression: &str, contexts: &[Vec<u8>]) -> napi::Result<Vec<u32>> {
    let compiled = zen_expression::compile_expression(expression)
        .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;
    let roots = zen_expression::expression_root_references(expression)
        .map(|roots| roots.into_iter().collect::<std::collections::HashSet<_>>());

    let mut vm = zen_expression::vm::VM::new();
    let mut matches = Vec::new();
    for (index, payload) in contexts.iter().enumerate() {
        let Some(context) = parse_context(payload, roots.as_ref()) else {
            continue;
        };
        if matches!(
            compiled.evaluate_with(context, &mut vm),
            Ok(zen_expression::Variable::Bool(true))
        ) {
            matches.push(index as u32);
        }
    }
    Ok(matches)
}

/// Root-level context keys the expression can read — `null` when it addresses
/// the whole context or cannot be analyzed. Lets callers prune columns before
/// data is ever composed or serialized.
#[napi]
pub fn expression_root_references(expression: String) -> Option<Vec<String>> {
    zen_expression::expression_root_references(&expression)
}

/// Filters raw JSON context buffers through ONE compiled expression — the
/// expression compiles once and rows never exist as JS objects, so the
/// per-row boundary conversion that dominates `evaluateExpressionSync`
/// disappears. Rows that fail to parse or evaluate simply don't match.
#[napi]
pub async fn evaluate_expression_many(
    expression: String,
    contexts: Vec<Buffer>,
) -> napi::Result<Uint32Array> {
    let payloads: Vec<Vec<u8>> = contexts.iter().map(|buffer| buffer.to_vec()).collect();
    let matches = spawn_worker(move || async move { matching_indices(&expression, &payloads) })
        .await
        .map_err(|_| anyhow!("Hook timed out"))??;
    Ok(Uint32Array::new(matches))
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
            let mut value = serde_json::to_value(result)
                .map_err(|e| napi::Error::from_reason(e.to_string()))?;
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

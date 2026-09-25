use std::collections::HashMap;
use std::rc::Rc;

use napi::Env;
use napi_derive::napi;
use serde_json::{json, Value};
use zen_engine::workspace::{self, CursorScope, ExpressionKind, SlotResponse, SlotRole};
use zen_expression::intellisense::IntelliSense;
use zen_expression::slot::LabelResolver;
use zen_expression::variable::VariableType;

use crate::expression::json_to_variable_type;
use crate::policy::{variable_type_to_json, PolicyExpressionCursor, Workspace};

#[napi]
impl Workspace {
    #[napi(ts_return_type = "PolicySlotResponse | null")]
    pub fn slot(
        &self,
        env: Env,
        cursor: PolicyExpressionCursor,
        text: String,
    ) -> napi::Result<Option<Value>> {
        self.ensure_function_types(&env)?;
        let cursor: workspace::Cursor = cursor.try_into()?;
        self.inner
            .slot(&cursor, &text)
            .map(slot_response_json)
            .transpose()
    }

    #[napi(ts_return_type = "PolicyExpressionFacts[]")]
    pub fn facts(&self, env: Env, policy_path: String) -> napi::Result<Vec<Value>> {
        self.ensure_function_types(&env)?;
        self.inner
            .facts(&policy_path)
            .iter()
            .map(|facts| {
                let mut value = to_json(facts)?;
                let obj = object_mut(&mut value)?;
                obj.insert("subjectType".into(), type_json(&facts.subject_type));
                obj.insert("expectedType".into(), type_json(&facts.expected_type));
                Ok(value)
            })
            .collect()
    }
}

#[napi(object)]
pub struct SlotRequest {
    pub id: String,
    pub text: String,
    pub pos: u32,
    pub unary: bool,
    #[napi(ts_type = "PolicySlotRole")]
    pub role: String,
    #[napi(ts_type = "PolicyVariableType")]
    pub scope: Value,
    #[napi(ts_type = "PolicyVariableType | null")]
    pub expected: Option<Value>,
    #[napi(ts_type = "Record<string, Record<string, string>> | null")]
    pub labels: Option<Value>,
}

#[napi(ts_return_type = "Array<{ id: string; result: PolicySlotResponse }>")]
pub fn slot_batch(requests: Vec<SlotRequest>, strict: Option<bool>) -> napi::Result<Vec<Value>> {
    let mut is = IntelliSense::new().with_strict(strict.unwrap_or(false));
    let mut out = Vec::with_capacity(requests.len());
    for request in requests {
        let role = parse_role(&request.role)?;
        let scope = CursorScope {
            kind: if request.unary {
                ExpressionKind::Unary
            } else {
                ExpressionKind::Standard
            },
            role,
            scope: json_to_variable_type(&request.scope),
            expected: request.expected.as_ref().map(json_to_variable_type),
            inferred: false,
        };
        is.set_labels(label_resolver(request.labels.as_ref()));
        let response = SlotResponse::compute(&mut is, &scope, &request.text, request.pos);
        out.push(serde_json::json!({
            "id": request.id,
            "result": slot_response_json(response)?,
        }));
    }
    is.set_labels(None);
    Ok(out)
}

fn parse_role(role: &str) -> napi::Result<SlotRole> {
    match role {
        "unary" => Ok(SlotRole::Unary),
        "condition" => Ok(SlotRole::Condition),
        "value" => Ok(SlotRole::Value),
        "path" => Ok(SlotRole::Path),
        other => Err(napi::Error::from_reason(format!(
            "invalid slot role: {other}"
        ))),
    }
}

fn label_resolver(labels: Option<&Value>) -> Option<LabelResolver> {
    let map: HashMap<String, HashMap<String, String>> =
        serde_json::from_value(labels?.clone()).ok()?;
    if map.is_empty() {
        return None;
    }
    Some(Rc::new(move |name: &str, value: &str| {
        map.get(name)?.get(value).cloned()
    }))
}

fn slot_response_json(response: SlotResponse) -> napi::Result<Value> {
    let mut value = to_json(&response)?;
    let obj = object_mut(&mut value)?;
    obj.insert("subjectType".into(), type_json(&response.subject_type));
    obj.insert("expectedType".into(), type_json(&response.expected_type));
    let slot = obj
        .get_mut("slot")
        .ok_or_else(|| napi::Error::from_reason("slot response has no slot"))?;
    let slot = object_mut(slot)?;
    slot.insert("expected".into(), type_json(&response.slot.expected));
    slot.insert("operand".into(), type_json(&response.slot.operand));
    let locals = response
        .slot
        .locals
        .iter()
        .map(|l| json!({ "name": l.name, "type": variable_type_to_json(&l.kind) }))
        .collect();
    slot.insert("locals".into(), Value::Array(locals));
    Ok(value)
}

fn to_json<T: serde::Serialize>(value: &T) -> napi::Result<Value> {
    serde_json::to_value(value).map_err(|e| napi::Error::from_reason(e.to_string()))
}

fn object_mut(value: &mut Value) -> napi::Result<&mut serde_json::Map<String, Value>> {
    value
        .as_object_mut()
        .ok_or_else(|| napi::Error::from_reason("expected a JSON object"))
}

fn type_json(t: &Option<VariableType>) -> Value {
    t.as_ref().map(variable_type_to_json).unwrap_or(Value::Null)
}

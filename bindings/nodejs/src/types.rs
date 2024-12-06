use json_dotpath::DotPaths;
use napi::anyhow::{anyhow, Context};
use napi_derive::napi;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use zen_engine::handler::custom_node_adapter::CustomDecisionNode;
use zen_engine::{DecisionGraphResponse, DecisionGraphTrace};
use zen_expression::Variable;

#[napi(object)]
pub struct ZenEngineTrace {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub output: Value,
    pub performance: Option<String>,
    pub trace_data: Option<Value>,
    pub order: u32,
}

impl From<DecisionGraphTrace> for ZenEngineTrace {
    fn from(value: DecisionGraphTrace) -> Self {
        Self {
            id: value.id,
            name: value.name,
            input: value.input.to_value(),
            output: value.output.to_value(),
            performance: value.performance,
            trace_data: value.trace_data,
            order: value.order,
        }
    }
}

#[napi(object)]
pub struct ZenEngineResponse {
    pub performance: String,
    pub result: Value,
    pub trace: Option<HashMap<String, ZenEngineTrace>>,
}

impl From<DecisionGraphResponse> for ZenEngineResponse {
    fn from(value: DecisionGraphResponse) -> Self {
        Self {
            performance: value.performance,
            result: value.result.to_value(),
            trace: value.trace.map(|opt| {
                opt.into_iter()
                    .map(|(key, value)| (key, ZenEngineTrace::from(value)))
                    .collect()
            }),
        }
    }
}

#[napi(object)]
pub struct ZenEngineHandlerResponse {
    pub output: Value,
    pub trace_data: Option<Value>,
}

#[derive(Clone)]
#[napi(object)]
pub struct DecisionNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub config: Arc<Value>,
}

impl From<CustomDecisionNode> for DecisionNode {
    fn from(value: CustomDecisionNode) -> Self {
        Self {
            id: value.id,
            name: value.name,
            kind: value.kind,
            config: value.config,
        }
    }
}

#[napi]
pub struct ZenEngineHandlerRequest {
    pub input: Value,
    pub node: DecisionNode,
}

#[napi]
impl ZenEngineHandlerRequest {
    #[napi(constructor)]
    pub fn new() -> napi::Result<Self> {
        Err(anyhow!("Private constructor").into())
    }

    #[napi(ts_return_type = "unknown")]
    pub fn get_field(&self, path: String) -> napi::Result<Value> {
        let node_config = &self.node.config;

        let selected_value: Value = node_config
            .dot_get(path.as_str())
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;
        let Value::String(template) = selected_value else {
            return Ok(selected_value);
        };

        let template_value = zen_tmpl::render(template.as_str(), Variable::from(&self.input))
            .map_err(|e| anyhow!(serde_json::to_string(&e).unwrap_or_else(|_| e.to_string())))?;

        Ok(template_value.to_value())
    }

    #[napi(ts_return_type = "unknown")]
    pub fn get_field_raw(&self, path: String) -> napi::Result<Value> {
        let node_config = &self.node.config;

        let selected_value: Value = node_config
            .dot_get(path.as_str())
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;

        Ok(selected_value.clone())
    }
}

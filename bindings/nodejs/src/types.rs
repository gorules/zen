use std::collections::HashMap;

use json_dotpath::DotPaths;
use napi::anyhow::{anyhow, Context};
use napi::JsObject;
use napi_derive::napi;
use serde_json::Value;

use zen_engine::model::DecisionNodeKind;
use zen_engine::{DecisionGraphResponse, DecisionGraphTrace};

#[napi(object)]
pub struct ZenEngineTrace {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub output: Value,
    pub performance: Option<String>,
    pub trace_data: Option<Value>,
}

impl From<DecisionGraphTrace> for ZenEngineTrace {
    fn from(value: DecisionGraphTrace) -> Self {
        Self {
            id: value.id,
            name: value.name,
            input: value.input,
            output: value.output,
            performance: value.performance,
            trace_data: value.trace_data,
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
            result: value.result,
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
    pub output: JsObject,
    pub trace_data: Option<JsObject>,
}

#[derive(Clone)]
#[napi(object)]
pub struct DecisionNode {
    pub id: String,
    pub name: String,
    #[napi(js_name = "type")]
    pub kind: String,
    pub content: CustomNodeContent,
}

impl TryFrom<zen_engine::model::DecisionNode> for DecisionNode {
    type Error = ();

    fn try_from(value: zen_engine::model::DecisionNode) -> Result<Self, Self::Error> {
        let DecisionNodeKind::CustomNode { content } = value.kind else {
            return Err(());
        };

        Ok(Self {
            id: value.id,
            name: value.name,
            kind: String::from("customNode"),
            content: CustomNodeContent {
                component: content.component,
                config: content.config,
            },
        })
    }
}

#[derive(Clone)]
#[napi(object)]
pub struct CustomNodeContent {
    pub component: String,
    /// Config is where custom data is kept. Usually in JSON format.
    pub config: Value,
}

#[napi]
pub struct ZenEngineHandlerRequest {
    pub input: Value,
    pub node: DecisionNode,
    pub iteration: u8,
}

#[napi]
impl ZenEngineHandlerRequest {
    #[napi(constructor)]
    pub fn new() -> napi::Result<Self> {
        Err(anyhow!("Private constructor").into())
    }

    #[napi(ts_return_type = "unknown")]
    pub fn get_field(&self, path: String) -> napi::Result<Value> {
        let node_config = &self.node.content.config;

        let selected_value: Value = node_config
            .dot_get(path.as_str())
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;
        let Value::String(template) = selected_value else {
            return Ok(selected_value.clone());
        };

        Ok(zen_template::render(template.as_str(), &self.input))
    }

    #[napi(ts_return_type = "unknown")]
    pub fn get_field_raw(&self, path: String) -> napi::Result<Value> {
        let node_config = &self.node.content.config;

        let selected_value: Value = node_config
            .dot_get(path.as_str())
            .ok()
            .flatten()
            .context("Failed to find JSON path")?;

        Ok(selected_value.clone())
    }
}

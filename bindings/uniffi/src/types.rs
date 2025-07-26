use crate::error::ZenError;
use serde_json::Value;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use zen_engine::handler::custom_node_adapter::CustomDecisionNode;
use zen_engine::{DecisionGraphResponse, DecisionGraphTrace};
use zen_expression::Variable;

#[derive(uniffi::Object)]
pub struct JsonBuffer(pub(crate) Value);

impl From<JsonBuffer> for Value {
    fn from(value: JsonBuffer) -> Self {
        value.0
    }
}

impl From<JsonBuffer> for Variable {
    fn from(value: JsonBuffer) -> Variable {
        Variable::from(&value.0)
    }
}

impl From<Value> for JsonBuffer {
    fn from(value: Value) -> Self {
        JsonBuffer(value)
    }
}

impl From<Variable> for JsonBuffer {
    fn from(var: Variable) -> Self {
        JsonBuffer(var.to_value())
    }
}

impl JsonBuffer {
    pub fn to_value(&self) -> Value {
        self.0.clone()
    }

    pub fn to_variable(&self) -> Variable {
        Variable::from(&self.0)
    }
}

#[uniffi::export]
impl JsonBuffer {
    #[uniffi::constructor]
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ZenError> {
        serde_json::from_slice(&bytes)
            .map(JsonBuffer)
            .map_err(|_| ZenError::JsonDeserializationFailed)
    }

    #[uniffi::constructor]
    pub fn from_string(json_str: String) -> Result<Self, ZenError> {
        serde_json::from_str(&json_str)
            .map(JsonBuffer)
            .map_err(|_| ZenError::JsonDeserializationFailed)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, ZenError> {
        serde_json::to_vec(&self.0).map_err(|_| ZenError::JsonSerializationFailed)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.0).unwrap_or_else(|_| String::from("Failed to serialize JSON"))
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineTrace {
    pub id: String,
    pub name: String,
    pub input: Arc<JsonBuffer>,
    pub output: Arc<JsonBuffer>,
    pub performance: Option<String>,
    pub trace_data: Option<Arc<JsonBuffer>>,
    pub order: u32,
}

impl From<DecisionGraphTrace> for ZenEngineTrace {
    fn from(value: DecisionGraphTrace) -> Self {
        Self {
            id: value.id,
            name: value.name,
            input: Arc::new(value.input.into()),
            output: Arc::new(value.output.into()),
            performance: value.performance,
            trace_data: value.trace_data.map(|data| Arc::new(data.into())),
            order: value.order,
        }
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineResponse {
    pub performance: String,
    pub result: Arc<JsonBuffer>,
    pub trace: Option<HashMap<String, ZenEngineTrace>>,
}

impl From<DecisionGraphResponse> for ZenEngineResponse {
    fn from(value: DecisionGraphResponse) -> Self {
        Self {
            performance: value.performance,
            result: Arc::new(value.result.into()),
            trace: value.trace.map(|opt| {
                opt.into_iter()
                    .map(|(key, value)| (key, ZenEngineTrace::from(value)))
                    .collect()
            }),
        }
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineHandlerResponse {
    pub output: Arc<JsonBuffer>,
    pub trace_data: Option<Arc<JsonBuffer>>,
}

#[derive(uniffi::Record)]
pub struct DecisionNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub config: Arc<JsonBuffer>,
}

impl From<CustomDecisionNode> for DecisionNode {
    fn from(value: CustomDecisionNode) -> Self {
        Self {
            id: value.id,
            name: value.name,
            kind: value.kind,
            config: Arc::new(value.config.deref().clone().into()),
        }
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineHandlerRequest {
    pub input: Arc<JsonBuffer>,
    pub node: DecisionNode,
}

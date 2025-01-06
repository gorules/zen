use crate::error::ZenError;
use serde_json::Value;
use std::collections::HashMap;
use zen_engine::handler::custom_node_adapter::CustomDecisionNode;
use zen_engine::{DecisionGraphResponse, DecisionGraphTrace};

pub struct JsonBuffer(pub Vec<u8>);
uniffi::custom_newtype!(JsonBuffer, Vec<u8>);

impl TryFrom<JsonBuffer> for Value {
    type Error = ZenError;

    fn try_from(value: JsonBuffer) -> Result<Self, Self::Error> {
        serde_json::from_slice(&value.0).map_err(|_| ZenError::JsonDeserializationFailed)
    }
}

impl TryFrom<Value> for JsonBuffer {
    type Error = ZenError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        serde_json::to_vec(&value)
            .map(|v| JsonBuffer(v))
            .map_err(|_| ZenError::JsonSerializationFailed)
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineTrace {
    pub id: String,
    pub name: String,
    pub input: JsonBuffer,
    pub output: JsonBuffer,
    pub performance: Option<String>,
    pub trace_data: Option<JsonBuffer>,
    pub order: u32,
}

impl From<DecisionGraphTrace> for ZenEngineTrace {
    fn from(value: DecisionGraphTrace) -> Self {
        Self {
            id: value.id,
            name: value.name,
            input: value.input.to_value().try_into().unwrap(),
            output: value.output.to_value().try_into().unwrap(),
            performance: value.performance,
            trace_data: value.trace_data.map(|data| data.try_into().unwrap()),
            order: value.order,
        }
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineResponse {
    pub performance: String,
    pub result: JsonBuffer,
    pub trace: Option<HashMap<String, ZenEngineTrace>>,
}

impl From<DecisionGraphResponse> for ZenEngineResponse {
    fn from(value: DecisionGraphResponse) -> Self {
        Self {
            performance: value.performance,
            result: value.result.to_value().try_into().unwrap(),
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
    pub output: JsonBuffer,
    pub trace_data: Option<JsonBuffer>,
}

#[derive(uniffi::Record)]
pub struct DecisionNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub config: JsonBuffer,
}

impl From<CustomDecisionNode> for DecisionNode {
    fn from(value: CustomDecisionNode) -> Self {
        Self {
            id: value.id,
            name: value.name,
            kind: value.kind,
            config: JsonBuffer(serde_json::to_vec(&value.config).unwrap()),
        }
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineHandlerRequest {
    pub input: JsonBuffer,
    pub node: DecisionNode,
}

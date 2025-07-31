use crate::error::ZenError;
use serde_json::Value;
use std::collections::HashMap;
use zen_engine::handler::custom_node_adapter::CustomDecisionNode;
use zen_engine::{DecisionGraphResponse, DecisionGraphTrace};
use zen_expression::Variable;

pub struct JsonBuffer(pub Vec<u8>);
uniffi::custom_newtype!(JsonBuffer, Vec<u8>);

impl TryFrom<JsonBuffer> for Value {
    type Error = ZenError;

    fn try_from(value: JsonBuffer) -> Result<Self, Self::Error> {
        serde_json::from_slice(&value.0).map_err(|_| ZenError::JsonDeserializationFailed)
    }
}

impl TryFrom<JsonBuffer> for Variable {
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

impl TryFrom<Variable> for JsonBuffer {
    type Error = ZenError;

    fn try_from(var: Variable) -> Result<Self, Self::Error> {
        serde_json::to_vec(&var)
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

impl TryFrom<DecisionGraphTrace> for ZenEngineTrace {
    type Error = ZenError;

    fn try_from(value: DecisionGraphTrace) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            name: value.name,
            input: JsonBuffer::try_from(value.input)?,
            output: JsonBuffer::try_from(value.output)?,
            performance: value.performance,
            trace_data: value.trace_data.map(JsonBuffer::try_from).transpose()?,
            order: value.order,
        })
    }
}

#[derive(uniffi::Record)]
pub struct ZenEngineResponse {
    pub performance: String,
    pub result: JsonBuffer,
    pub trace: Option<HashMap<String, ZenEngineTrace>>,
}

impl TryFrom<DecisionGraphResponse> for ZenEngineResponse {
    type Error = ZenError;

    fn try_from(value: DecisionGraphResponse) -> Result<Self, Self::Error> {
        Ok(Self {
            performance: value.performance,
            result: JsonBuffer::try_from(value.result)?,
            trace: value
                .trace
                .map(|opt| {
                    opt.into_iter()
                        .map(|(key, value)| Ok((key, ZenEngineTrace::try_from(value)?)))
                        .collect::<Result<_, ZenError>>()
                })
                .transpose()?,
        })
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

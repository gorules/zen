use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionContent {
    pub nodes: Vec<DecisionNode>,
    pub edges: Vec<DecisionEdge>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionEdge {
    pub source_id: String,
    pub target_id: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionNode {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    #[serde(flatten)]
    pub kind: DecisionNodeKind,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum DecisionNodeKind {
    InputNode,
    OutputNode,
    FunctionNode { content: String },
    DecisionNode { content: DecisionNodeContent },
    DecisionTableNode { content: DecisionTableContent },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionNodeContent {
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableContent {
    pub rules: Vec<HashMap<String, String>>,
    pub inputs: Vec<DecisionTableInputField>,
    pub outputs: Vec<DecisionTableOutputField>,
    pub hit_policy: DecisionTableHitPolicy,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub enum DecisionTableHitPolicy {
    First,
    Collect,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableInputField {
    pub id: String,
    pub name: String,
    pub field: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableOutputField {
    pub id: String,
    pub name: String,
    pub field: String,
}

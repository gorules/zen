use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};

/// JDM Decision model
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
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub source_handle: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionNode {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    #[serde(flatten)]
    pub kind: DecisionNodeKind,
}

impl PartialEq for DecisionNode {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
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
    ExpressionNode { content: ExpressionNodeContent },
    SwitchNode { content: SwitchNodeContent },
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
    pub field: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableOutputField {
    pub id: String,
    pub name: String,
    pub field: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct ExpressionNodeContent {
    pub expressions: Vec<Expression>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct Expression {
    pub id: String,
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct SwitchNodeContent {
    #[serde(default)]
    pub mode: SwitchNodeMode,
    pub statements: Vec<SwitchStatement>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub struct SwitchStatement {
    pub id: String,
    pub condition: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[serde(rename_all = "camelCase")]
pub enum SwitchNodeMode {
    #[default]
    First,
    Collect,
}

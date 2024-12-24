use ahash::HashMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::sync::Arc;

/// JDM Decision model
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionContent {
    pub nodes: Vec<Arc<DecisionNode>>,
    pub edges: Vec<Arc<DecisionEdge>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionEdge {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub source_handle: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum DecisionNodeKind {
    InputNode {
        #[serde(default)]
        content: InputNodeContent,
    },
    OutputNode {
        #[serde(default)]
        content: OutputNodeContent,
    },
    FunctionNode {
        content: FunctionNodeContent,
    },
    DecisionNode {
        content: DecisionNodeContent,
    },
    DecisionTableNode {
        content: DecisionTableContent,
    },
    ExpressionNode {
        content: ExpressionNodeContent,
    },
    SwitchNode {
        content: SwitchNodeContent,
    },
    CustomNode {
        content: CustomNodeContent,
    },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InputNodeContent {
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub schema: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OutputNodeContent {
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub schema: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
pub enum FunctionNodeContent {
    Version2(FunctionContent),
    Version1(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FunctionContent {
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionNodeContent {
    pub key: String,
    #[serde(flatten)]
    pub transform_attributes: TransformAttributes,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableContent {
    pub rules: Vec<HashMap<String, String>>,
    pub inputs: Vec<DecisionTableInputField>,
    pub outputs: Vec<DecisionTableOutputField>,
    pub hit_policy: DecisionTableHitPolicy,
    #[serde(flatten)]
    pub transform_attributes: TransformAttributes,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DecisionTableHitPolicy {
    First,
    Collect,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableInputField {
    pub id: String,
    pub name: String,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub field: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableOutputField {
    pub id: String,
    pub name: String,
    pub field: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionNodeContent {
    pub expressions: Vec<Expression>,
    #[serde(flatten)]
    pub transform_attributes: TransformAttributes,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Expression {
    pub id: String,
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchNodeContent {
    #[serde(default)]
    pub hit_policy: SwitchStatementHitPolicy,
    pub statements: Vec<SwitchStatement>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchStatement {
    pub id: String,
    pub condition: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SwitchStatementHitPolicy {
    #[default]
    First,
    Collect,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TransformAttributes {
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub input_field: Option<String>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub output_path: Option<String>,
    #[serde(default)]
    pub execution_mode: TransformExecutionMode,
    #[serde(default)]
    pub pass_through: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TransformExecutionMode {
    #[default]
    Single,
    Loop,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CustomNodeContent {
    pub kind: String,
    pub config: Arc<Value>,
}

fn empty_string_is_none<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNull {
        String(String),
        Null,
    }

    match StringOrNull::deserialize(deserializer)? {
        StringOrNull::String(s) if s.trim().is_empty() => Ok(None),
        StringOrNull::String(s) => Ok(Some(s)),
        StringOrNull::Null => Ok(None),
    }
}

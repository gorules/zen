use ahash::HashMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionEdge {
    pub id: Arc<str>,
    pub source_id: Arc<str>,
    pub target_id: Arc<str>,
    pub source_handle: Option<Arc<str>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionNode {
    pub id: Arc<str>,
    pub name: Arc<str>,
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
    #[serde(default, deserialize_with = "empty_value_string_is_none_safe")]
    pub schema: Option<Arc<Value>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OutputNodeContent {
    #[serde(default, deserialize_with = "empty_value_string_is_none_safe")]
    pub schema: Option<Arc<Value>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
pub enum FunctionNodeContent {
    Version2(FunctionContent),
    Version1(Arc<str>),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FunctionContent {
    pub source: Arc<str>,
    #[serde(default)]
    pub omit_nodes: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionNodeContent {
    pub key: Arc<str>,
    #[serde(flatten)]
    pub transform_attributes: TransformAttributes,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableContent {
    #[serde(deserialize_with = "deserialize_trim_rules")]
    pub rules: Arc<Vec<HashMap<Arc<str>, Arc<str>>>>,
    pub inputs: Arc<Vec<DecisionTableInputField>>,
    pub outputs: Arc<Vec<DecisionTableOutputField>>,
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
    pub id: Arc<str>,
    pub name: Arc<str>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub field: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableOutputField {
    pub id: Arc<str>,
    pub name: Arc<str>,
    pub field: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionNodeContent {
    pub expressions: Arc<Vec<Expression>>,
    #[serde(flatten)]
    pub transform_attributes: TransformAttributes,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Expression {
    pub id: Arc<str>,
    pub key: Arc<str>,
    pub value: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchNodeContent {
    #[serde(default)]
    pub hit_policy: SwitchStatementHitPolicy,
    pub statements: Arc<Vec<SwitchStatement>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchStatement {
    pub id: Arc<str>,
    pub condition: Arc<str>,
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
    pub input_field: Option<Arc<str>>,
    #[serde(default, deserialize_with = "empty_string_is_none")]
    pub output_path: Option<Arc<str>>,
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
    pub kind: Arc<str>,
    pub config: Arc<Value>,
}

fn empty_string_is_none<'de, D>(deserializer: D) -> Result<Option<Arc<str>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNull {
        String(Arc<str>),
        Null,
    }

    match StringOrNull::deserialize(deserializer)? {
        StringOrNull::String(s) if s.trim().is_empty() => Ok(None),
        StringOrNull::String(s) => Ok(Some(s)),
        StringOrNull::Null => Ok(None),
    }
}

fn empty_value_string_is_none_safe<'de, D>(deserializer: D) -> Result<Option<Arc<Value>>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = empty_string_is_none(deserializer)?;
    let Some(data) = s else {
        return Ok(None);
    };

    Ok(serde_json::from_str(data.as_ref()).ok())
}

fn deserialize_trim_rules<'de, D>(
    deserializer: D,
) -> Result<Arc<Vec<HashMap<Arc<str>, Arc<str>>>>, D::Error>
where
    D: Deserializer<'de>,
{
    let rules: Vec<HashMap<Arc<str>, Arc<str>>> = Vec::deserialize(deserializer)?;

    let filtered_rules: Vec<HashMap<Arc<str>, Arc<str>>> = rules
        .into_iter()
        .map(|rule| {
            rule.into_iter()
                .map(|(k, v)| (k, Arc::from(v.trim())))
                .collect()
        })
        .collect();

    Ok(Arc::new(filtered_rules))
}

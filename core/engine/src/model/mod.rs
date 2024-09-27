use ahash::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JDM Decision model
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionContent {
    pub nodes: Vec<DecisionNode>,
    pub edges: Vec<DecisionEdge>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionEdge {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub source_handle: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
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
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum DecisionNodeKind {
    InputNode,
    OutputNode,
    FunctionNode { content: FunctionNodeContent },
    DecisionNode { content: DecisionNodeContent },
    DecisionTableNode { content: DecisionTableContent },
    ExpressionNode { content: ExpressionNodeContent },
    SwitchNode { content: SwitchNodeContent },
    CustomNode { content: CustomNodeContent },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
pub enum FunctionNodeContent {
    Version2(FunctionContent),
    Version1(String),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct FunctionContent {
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionNodeContent {
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableContent {
    pub rules: Vec<HashMap<String, String>>,
    pub inputs: Vec<DecisionTableInputField>,
    pub outputs: Vec<DecisionTableOutputField>,
    pub hit_policy: DecisionTableHitPolicy,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub enum DecisionTableHitPolicy {
    First,
    Collect,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableInputField {
    pub id: String,
    pub name: String,
    pub field: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct DecisionTableOutputField {
    pub id: String,
    pub name: String,
    pub field: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct ExpressionNodeContent {
    pub expressions: Vec<Expression>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct Expression {
    pub id: String,
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct SwitchNodeContent {
    #[serde(default)]
    pub hit_policy: SwitchStatementHitPolicy,
    pub statements: Vec<SwitchStatement>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub struct SwitchStatement {
    pub id: String,
    pub condition: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[cfg_attr(feature = "bincode", derive(bincode::Encode, bincode::Decode))]
#[serde(rename_all = "camelCase")]
pub enum SwitchStatementHitPolicy {
    #[default]
    First,
    Collect,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CustomNodeContent {
    pub kind: String,
    pub config: Value,
}

#[cfg(feature = "bincode")]
impl ::bincode::Encode for CustomNodeContent {
    fn encode<__E: ::bincode::enc::Encoder>(
        &self,
        encoder: &mut __E,
    ) -> Result<(), ::bincode::error::EncodeError> {
        let config_string = self.config.to_string();

        ::bincode::Encode::encode(&self.kind, encoder)?;
        ::bincode::Encode::encode(config_string.as_bytes(), encoder)?;
        Ok(())
    }
}

#[cfg(feature = "bincode")]
impl ::bincode::Decode for CustomNodeContent {
    fn decode<__D: ::bincode::de::Decoder>(
        decoder: &mut __D,
    ) -> Result<Self, ::bincode::error::DecodeError> {
        let kind: String = ::bincode::Decode::decode(decoder)?;
        let config_string: String = ::bincode::Decode::decode(decoder)?;

        let config = serde_json::from_str(config_string.as_str())
            .map_err(|_| ::bincode::error::DecodeError::Other("failed to deserialize value"))?;

        Ok(Self { kind, config })
    }
}

#[cfg(feature = "bincode")]
impl<'__de> ::bincode::BorrowDecode<'__de> for CustomNodeContent {
    fn borrow_decode<__D: ::bincode::de::BorrowDecoder<'__de>>(
        decoder: &mut __D,
    ) -> Result<Self, ::bincode::error::DecodeError> {
        let kind: String = ::bincode::BorrowDecode::borrow_decode(decoder)?;
        let config_string: String = ::bincode::BorrowDecode::borrow_decode(decoder)?;

        let config = serde_json::from_str(config_string.as_str())
            .map_err(|_| ::bincode::error::DecodeError::Other("failed to deserialize value"))?;

        Ok(Self { kind, config })
    }
}

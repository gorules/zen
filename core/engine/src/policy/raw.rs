use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::policy::blocks::{AssertionDoc, DecisionTableDoc, ExpressionDoc, MatchDoc};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDocument {
    #[serde(default)]
    pub imports: Vec<Arc<str>>,
    pub blocks: Vec<BlockDoc>,
}

#[derive(Debug, Clone)]
pub enum BlockDoc {
    Assertion {
        id: Arc<str>,
        data: AssertionDoc,
    },
    DecisionTable {
        id: Arc<str>,
        data: DecisionTableDoc,
    },
    Expression {
        id: Arc<str>,
        data: ExpressionDoc,
    },
    Match {
        id: Arc<str>,
        data: MatchDoc,
    },
    DataModel {
        id: Arc<str>,
        data: DataModelDoc,
    },
    Ignored(serde_json::Value),
}

impl BlockDoc {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Assertion { id, .. }
            | Self::DecisionTable { id, .. }
            | Self::Expression { id, .. }
            | Self::Match { id, .. }
            | Self::DataModel { id, .. } => Some(id),
            Self::Ignored(value) => value.get("id").and_then(serde_json::Value::as_str),
        }
    }

    fn decode_known(tag: BlockTag, value: serde_json::Value) -> Result<Self, serde_json::Error> {
        use serde::de::Error;

        let BlockEnvelope { id, props } = serde_json::from_value(value)?;
        let data = props
            .data
            .ok_or_else(|| serde_json::Error::missing_field("data"))?;

        match tag {
            BlockTag::Assertion => Ok(Self::Assertion {
                id,
                data: serde_json::from_value(data)?,
            }),
            BlockTag::DecisionTable => Ok(Self::DecisionTable {
                id,
                data: DecisionTableDoc::decode_wire(data).map_err(serde_json::Error::custom)?,
            }),
            BlockTag::Expression => Ok(Self::Expression {
                id,
                data: serde_json::from_value(data)?,
            }),
            BlockTag::Match => Ok(Self::Match {
                id,
                data: serde_json::from_value(data)?,
            }),
            BlockTag::DataModel => Ok(Self::DataModel {
                id,
                data: serde_json::from_value(data)?,
            }),
        }
    }
}

impl<'de> Deserialize<'de> for BlockDoc {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        let value = serde_json::Value::deserialize(deserializer)?;
        let tag = match value.get("type") {
            Some(serde_json::Value::String(name)) => BlockTag::from_name(name),
            Some(_) => return Err(Error::custom("block `type` must be a string")),
            None => return Err(Error::missing_field("type")),
        };

        match tag {
            Some(tag) => Self::decode_known(tag, value).map_err(Error::custom),
            None => Ok(Self::Ignored(value)),
        }
    }
}

impl Serialize for BlockDoc {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Assertion { id, data } => {
                TaggedBlockRef::new(BlockTag::Assertion, id, data).serialize(serializer)
            }
            Self::DecisionTable { id, data } => {
                TaggedBlockRef::new(BlockTag::DecisionTable, id, data).serialize(serializer)
            }
            Self::Expression { id, data } => {
                TaggedBlockRef::new(BlockTag::Expression, id, data).serialize(serializer)
            }
            Self::Match { id, data } => {
                TaggedBlockRef::new(BlockTag::Match, id, data).serialize(serializer)
            }
            Self::DataModel { id, data } => {
                TaggedBlockRef::new(BlockTag::DataModel, id, data).serialize(serializer)
            }
            Self::Ignored(value) => value.serialize(serializer),
        }
    }
}

#[derive(Clone, Copy)]
enum BlockTag {
    Assertion,
    DecisionTable,
    Expression,
    Match,
    DataModel,
}

impl BlockTag {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "assertion" => Some(Self::Assertion),
            "decisionTable" => Some(Self::DecisionTable),
            "expression" => Some(Self::Expression),
            "match" => Some(Self::Match),
            "dataModel" => Some(Self::DataModel),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Assertion => "assertion",
            Self::DecisionTable => "decisionTable",
            Self::Expression => "expression",
            Self::Match => "match",
            Self::DataModel => "dataModel",
        }
    }
}

#[derive(Deserialize)]
struct BlockEnvelope {
    id: Arc<str>,
    props: PropsEnvelope,
}

#[derive(Deserialize)]
struct PropsEnvelope {
    #[serde(default)]
    data: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct TaggedBlockRef<'a, T> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a Arc<str>,
    props: PropsRef<'a, T>,
}

impl<'a, T> TaggedBlockRef<'a, T> {
    fn new(tag: BlockTag, id: &'a Arc<str>, data: &'a T) -> Self {
        Self {
            kind: tag.name(),
            id,
            props: PropsRef { data },
        }
    }
}

#[derive(Serialize)]
struct PropsRef<'a, T> {
    data: &'a T,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataModelDoc {
    pub name: Arc<str>,
    #[serde(default)]
    pub scope: ScopeDoc,
    #[serde(default)]
    pub properties: Vec<PropertyDoc>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScopeDoc {
    #[default]
    Entity,
    Global,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDoc {
    pub id: Arc<str>,
    pub name: Arc<str>,
    #[serde(flatten)]
    pub property_type: PropertyTypeDoc,
    #[serde(default)]
    pub array: bool,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PropertyTypeDoc {
    String {
        #[serde(default, rename = "enum", skip_serializing_if = "Option::is_none")]
        values: Option<Vec<Arc<str>>>,
    },
    Number,
    Boolean,
    Date,
    Relationship {
        target: Arc<str>,
    },
    Reference {
        target: Arc<str>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_block_round_trips_losslessly() {
        let doc_json = serde_json::json!({
            "blocks": [
                {"type": "someLayoutBlock", "foo": 1}
            ]
        });

        let doc: PolicyDocument = serde_json::from_value(doc_json).unwrap();
        assert!(matches!(doc.blocks.as_slice(), [BlockDoc::Ignored(_)]));

        let serialized = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            serialized["blocks"][0],
            serde_json::json!({"type": "someLayoutBlock", "foo": 1})
        );
    }

    #[test]
    fn known_block_round_trips() {
        let block_json = serde_json::json!({
            "type": "expression",
            "id": "b1",
            "props": {"data": {"key": "a.b", "value": "1 + 1"}}
        });

        let block: BlockDoc = serde_json::from_value(block_json.clone()).unwrap();
        assert!(matches!(block, BlockDoc::Expression { .. }));

        let serialized = serde_json::to_value(&block).unwrap();
        assert_eq!(serialized, block_json);
    }

    #[test]
    fn ignored_block_exposes_id() {
        let with_id: BlockDoc =
            serde_json::from_value(serde_json::json!({"type": "someLayoutBlock", "id": "b1"}))
                .unwrap();
        assert_eq!(with_id.id(), Some("b1"));

        let without_id: BlockDoc =
            serde_json::from_value(serde_json::json!({"type": "someLayoutBlock"})).unwrap();
        assert_eq!(without_id.id(), None);

        let non_string_id: BlockDoc =
            serde_json::from_value(serde_json::json!({"type": "someLayoutBlock", "id": 1}))
                .unwrap();
        assert_eq!(non_string_id.id(), None);
    }

    #[test]
    fn upsert_by_id_replaces_ignored_block() {
        let mut doc: PolicyDocument = serde_json::from_value(serde_json::json!({
            "blocks": [
                {"type": "someLayoutBlock", "id": "b1"}
            ]
        }))
        .unwrap();

        let new_block: BlockDoc = serde_json::from_value(serde_json::json!({
            "type": "expression",
            "id": "b1",
            "props": {"data": {"key": "a.b", "value": "1 + 1"}}
        }))
        .unwrap();
        let new_id = new_block.id().unwrap().to_string();

        match doc
            .blocks
            .iter()
            .position(|b| b.id() == Some(new_id.as_str()))
        {
            Some(pos) => doc.blocks[pos] = new_block,
            None => doc.blocks.push(new_block),
        }

        assert_eq!(doc.blocks.len(), 1);
        assert!(matches!(doc.blocks[0], BlockDoc::Expression { .. }));
    }

    #[test]
    fn block_without_type_errors() {
        let missing = serde_json::json!({"id": "b1", "props": {"data": {}}});
        let non_string = serde_json::json!({"type": 1, "id": "b1"});

        assert!(serde_json::from_value::<BlockDoc>(missing).is_err());
        assert!(serde_json::from_value::<BlockDoc>(non_string).is_err());
    }

    #[test]
    fn known_block_with_bad_payload_errors() {
        let block_json = serde_json::json!({
            "type": "expression",
            "id": "b1",
            "props": {}
        });

        assert!(serde_json::from_value::<BlockDoc>(block_json).is_err());
    }
}

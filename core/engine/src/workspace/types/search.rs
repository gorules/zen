use std::sync::Arc;

use serde::Serialize;

use super::Span;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_id: Option<Arc<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<Arc<str>>,
    pub kind: SearchHitKind,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub span: Span,
    pub score: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchHitKind {
    Document,
    Heading,
    Paragraph,
    ListItem,
    CodeBlock,
    Expression,
    ExpressionKey,
    TableColumn,
    TableCell,
    MatchKey,
    MatchCondition,
    MatchValue,
    AssertionOutput,
    AssertionCondition,
    DataModel,
    DataModelProperty,
    Dictionary,
    DictionaryEntry,
    GraphNode,
    SwitchCondition,
    Function,
}

impl SearchHitKind {
    pub(crate) fn weight(self) -> u32 {
        match self {
            SearchHitKind::DataModel => 140,
            SearchHitKind::DataModelProperty => 135,
            SearchHitKind::Dictionary => 135,
            SearchHitKind::Document => 130,
            SearchHitKind::TableColumn => 130,
            SearchHitKind::GraphNode => 130,
            SearchHitKind::DictionaryEntry => 120,
            SearchHitKind::Heading => 120,
            SearchHitKind::ExpressionKey => 115,
            SearchHitKind::MatchKey => 115,
            SearchHitKind::AssertionOutput => 115,
            SearchHitKind::Paragraph => 100,
            SearchHitKind::ListItem => 100,
            SearchHitKind::Expression => 90,
            SearchHitKind::TableCell => 90,
            SearchHitKind::MatchCondition => 90,
            SearchHitKind::MatchValue => 90,
            SearchHitKind::AssertionCondition => 90,
            SearchHitKind::SwitchCondition => 90,
            SearchHitKind::CodeBlock => 60,
            SearchHitKind::Function => 60,
        }
    }
}

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zen_expression::variable::VariableType;

use super::Span;

pub use crate::workspace::slot::SlotRole;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cursor {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    pub pos: u32,
    pub target: CursorTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CursorTarget {
    Expression {
        id: Arc<str>,
    },
    AssertionOutput,
    ExpressionKey {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Arc<str>>,
    },
    MatchTarget,
    MatchValue {
        id: Arc<str>,
    },
    DecisionTableHead {
        col: Arc<str>,
    },
    DecisionTableCell {
        row: Arc<str>,
        col: Arc<str>,
    },
    DecisionTableRow {
        row: Arc<str>,
    },
    DataModelName,
    DataModelProperty {
        id: Arc<str>,
    },
    TransformInput,
    /// A data model property's `feature.expr`.
    FeatureExpr {
        id: Arc<str>,
    },
    /// A data model property's `compute`.
    ComputeExpr {
        id: Arc<str>,
    },
    /// The request path of a model input (`model.inputs[input]`).
    ModelInput {
        id: Arc<str>,
        input: Arc<str>,
    },
    /// A model's `model.when`.
    ModelWhen {
        id: Arc<str>,
    },
    /// What a call sends (`model.request`): empty sends the whole entity.
    ModelRequest {
        id: Arc<str>,
    },
    /// The call's value from its reply (`model.response`, reading `response`).
    ModelResponse {
        id: Arc<str>,
    },
}

impl CursorTarget {
    /// On an expression of a data model property (feature, compute, model).
    pub fn is_data_model_expression(&self) -> bool {
        matches!(
            self,
            CursorTarget::FeatureExpr { .. }
                | CursorTarget::ComputeExpr { .. }
                | CursorTarget::ModelInput { .. }
                | CursorTarget::ModelWhen { .. }
                | CursorTarget::ModelRequest { .. }
                | CursorTarget::ModelResponse { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExpressionKind {
    Standard,
    Unary,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectResult {
    pub span: Span,
    pub kind: VariableType,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareRename {
    pub target: RenameTarget,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RenameTarget {
    Entity {
        name: Arc<str>,
    },
    Field {
        entity: Arc<str>,
        field: Arc<str>,
    },
    Global {
        name: Arc<str>,
    },
    GraphProperty {
        document: Arc<str>,
        path: Arc<str>,
    },
    GraphNode {
        document: Arc<str>,
        node_id: Arc<str>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceSite {
    pub policy_path: Arc<str>,
    pub block_id: Arc<str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_id: Option<Arc<str>>,
    pub source: Arc<str>,
    pub span: Span,
    pub kind: ReferenceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReferenceKind {
    ExpressionRead,
    WriteKey,
    DataModel,
}

impl ReferenceKind {
    pub fn display_order(self) -> u8 {
        match self {
            ReferenceKind::DataModel => 0,
            ReferenceKind::ExpressionRead => 1,
            ReferenceKind::WriteKey => 2,
        }
    }
}

impl ReferenceSite {
    pub(crate) fn display_cmp(a: &Self, b: &Self) -> std::cmp::Ordering {
        a.kind
            .display_order()
            .cmp(&b.kind.display_order())
            .then_with(|| a.policy_path.cmp(&b.policy_path))
            .then_with(|| a.block_id.cmp(&b.block_id))
            .then_with(|| a.span.0.cmp(&b.span.0))
    }
}

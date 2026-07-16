use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EngineEdit {
    ReplaceBlock {
        policy_path: Arc<str>,
        block_id: Arc<str>,
        new_block: Value,
    },
    DeleteBlock {
        policy_path: Arc<str>,
        block_id: Arc<str>,
    },
    InsertBlock {
        policy_path: Arc<str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        after_block_id: Option<Arc<str>>,
        new_block: Value,
    },
    ReplaceNode {
        document: Arc<str>,
        node_id: Arc<str>,
        new_node: Value,
    },
}

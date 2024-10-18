use crate::model::DecisionNode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use thiserror::Error;
use zen_expression::variable::Variable;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeResponse {
    pub output: Variable,
    pub trace_data: Option<Value>,
}

#[derive(Debug, Serialize, Clone)]
pub struct NodeRequest {
    pub input: Variable,
    pub iteration: u8,
    pub node: Arc<DecisionNode>,
}

#[derive(Error, Debug)]
pub struct NodeError {
    pub node_id: String,
    #[source]
    pub source: anyhow::Error,
}

impl Display for NodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

pub type NodeResult = anyhow::Result<NodeResponse>;

use crate::model::DecisionNode;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;
use thiserror::Error;
use zen_expression::variable::Variable;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeResponse {
    pub output: Variable,
    pub trace_data: Option<Variable>,
}

#[derive(Debug, Serialize, Clone)]
pub struct NodeRequest {
    pub input: Variable,
    pub iteration: u8,
    pub node: Arc<DecisionNode>,
}

pub type NodeResult = Result<NodeResponse, NodeError>;

#[derive(Debug, Error)]
pub struct NodeError {
    pub node_id: Option<Arc<str>>,
    pub trace: Option<Variable>,
    pub source: Box<dyn std::error::Error>,
}

impl Display for NodeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

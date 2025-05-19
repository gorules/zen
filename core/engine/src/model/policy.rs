use crate::model::{DecisionTableHitPolicy, DecisionTableInputField, DecisionTableOutputField};
use ahash::HashMap;
use serde::{Deserialize, Serialize};
use std::iter::once;
use std::sync::Arc;

/// JDM Policy Model
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyBundle {
    pub documents: Vec<Arc<PolicyDocument>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum PolicyDocument {
    DecisionTable { content: Arc<PolicyDecisionTable> },
    RuleSet { content: Arc<PolicyRuleSet> },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecisionTable {
    pub rules: Vec<HashMap<String, String>>,
    pub inputs: Vec<DecisionTableInputField>,
    pub outputs: Vec<DecisionTableOutputField>,
    pub hit_policy: DecisionTableHitPolicy,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRuleSet {
    pub rules: Vec<Arc<PolicyRule>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRule {
    pub outcome: String,
    pub conditions: Option<PolicyRuleCondition>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum PolicyRuleCondition {
    Simple { expression: String },
    All { items: Vec<PolicyRuleCondition> },
    Any { items: Vec<PolicyRuleCondition> },
    None { items: Vec<PolicyRuleCondition> },
}

impl PolicyRuleCondition {
    pub fn to_vec(&self) -> Vec<&str> {
        match self {
            PolicyRuleCondition::Simple { expression } => once(expression.as_str())
                // .chain(self.to_vec().into_iter())
                .collect(),
            PolicyRuleCondition::All { items }
            | PolicyRuleCondition::Any { items }
            | PolicyRuleCondition::None { items } => {
                items.iter().map(|s| s.to_vec()).flatten().collect()
            }
        }
    }
}

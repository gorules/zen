use crate::policy::PolicyDocument;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Arc;
use zen_expression::{ExpressionKind, Isolate, OpcodeCache};
use zen_types::decision::{DecisionEdge, DecisionNode, DecisionNodeKind};

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum DecisionContent {
    Graph(GraphContent),
    Policy(PolicyContent),
}

impl<'de> Deserialize<'de> for DecisionContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let is_policy = value
            .as_object()
            .is_some_and(|object| object.contains_key("blocks"));

        let content = if is_policy {
            serde_path_to_error::deserialize::<_, PolicyContent>(value).map(Self::Policy)
        } else {
            serde_path_to_error::deserialize::<_, GraphContent>(value).map(Self::Graph)
        };

        content.map_err(serde::de::Error::custom)
    }
}

impl Default for DecisionContent {
    fn default() -> Self {
        Self::Graph(GraphContent::default())
    }
}

impl DecisionContent {
    pub fn as_graph(&self) -> Option<&GraphContent> {
        match self {
            Self::Graph(g) => Some(g),
            Self::Policy(_) => None,
        }
    }

    pub fn as_policy(&self) -> Option<&PolicyContent> {
        match self {
            Self::Policy(p) => Some(p),
            Self::Graph(_) => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Graph(_) => "graph",
            Self::Policy(_) => "policy",
        }
    }

    pub fn into_graph_arc(self: Arc<Self>) -> Option<Arc<GraphContent>> {
        match Arc::try_unwrap(self) {
            Ok(Self::Graph(g)) => Some(Arc::new(g)),
            Ok(Self::Policy(_)) => None,
            Err(arc) => match arc.as_ref() {
                Self::Graph(g) => Some(Arc::new(g.clone())),
                Self::Policy(_) => None,
            },
        }
    }
}

impl From<GraphContent> for DecisionContent {
    fn from(value: GraphContent) -> Self {
        Self::Graph(value)
    }
}

impl From<PolicyContent> for DecisionContent {
    fn from(value: PolicyContent) -> Self {
        Self::Policy(value)
    }
}

impl From<Arc<PolicyDocument>> for PolicyContent {
    fn from(value: Arc<PolicyDocument>) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GraphContent {
    pub nodes: Vec<Arc<DecisionNode>>,
    pub edges: Vec<Arc<DecisionEdge>>,

    #[serde(skip)]
    pub compiled_cache: Option<Arc<OpcodeCache>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PolicyContent(pub Arc<PolicyDocument>);

impl GraphContent {
    pub fn compile(&mut self) {
        if self.compiled_cache.is_some() {
            return;
        }

        let mut sources: Vec<(Arc<str>, ExpressionKind)> = Vec::new();
        for node in &self.nodes {
            match &node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    for expr in content.expressions.iter() {
                        if !expr.key.is_empty() && !expr.value.is_empty() {
                            sources.push((expr.value.clone(), ExpressionKind::Standard));
                        }
                    }
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    for rule in content.rules.iter() {
                        for input in content.inputs.iter() {
                            let Some(rule_value) = rule.get(&input.id) else {
                                continue;
                            };

                            let kind = if input.field.is_some() {
                                ExpressionKind::Unary
                            } else {
                                ExpressionKind::Standard
                            };

                            sources.push((rule_value.clone(), kind));
                        }

                        for output in content.outputs.iter() {
                            let Some(rule_value) = rule.get(&output.id) else {
                                continue;
                            };

                            sources.push((rule_value.clone(), ExpressionKind::Standard));
                        }
                    }
                }
                _ => {}
            }
        }

        let mut cache: OpcodeCache = OpcodeCache::new();
        let mut isolate = Isolate::new();

        for (source, kind) in &sources {
            let map = match kind {
                ExpressionKind::Standard => &mut cache.standard,
                ExpressionKind::Unary => &mut cache.unary,
            };
            if map.contains_key(source) {
                continue;
            }

            let result = match kind {
                ExpressionKind::Standard => isolate
                    .compile_standard(source)
                    .map(|e| e.bytecode().to_vec()),
                ExpressionKind::Unary => {
                    isolate.compile_unary(source).map(|e| e.bytecode().to_vec())
                }
            };
            if let Ok(bytecode) = result {
                map.insert(source.clone(), Arc::from(bytecode));
            }
        }

        self.compiled_cache.replace(Arc::new(cache));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_graph_error_mentions_field() {
        let error = serde_json::from_str::<DecisionContent>(r#"{"nodes":[],"edges":"bad"}"#)
            .unwrap_err()
            .to_string();
        assert!(error.contains("edges"), "{error}");
    }

    #[test]
    fn malformed_policy_error_mentions_inner_field() {
        let json = r#"{"blocks":[{"type":"assertion","id":"b1","props":{"data":{}}}]}"#;
        let error = serde_json::from_str::<DecisionContent>(json)
            .unwrap_err()
            .to_string();
        assert!(error.contains("output"), "{error}");
    }

    #[test]
    fn valid_graph_routes_to_graph_variant() {
        let content: DecisionContent = serde_json::from_str(r#"{"nodes":[],"edges":[]}"#).unwrap();
        assert!(content.as_graph().is_some());
    }

    #[test]
    fn valid_policy_routes_to_policy_variant() {
        let content: DecisionContent = serde_json::from_str(r#"{"blocks":[]}"#).unwrap();
        assert!(content.as_policy().is_some());
    }
}

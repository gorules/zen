use crate::loader::DecisionLoader;
use crate::model::{DecisionContent, DecisionNode, DecisionNodeKind};
use std::collections::HashMap;

use crate::handler::decision::DecisionHandler;
use crate::handler::function::FunctionHandler;
use crate::handler::node::NodeRequest;
use crate::handler::table::zen::DecisionTableHandler;

use crate::handler::expression::ExpressionHandler;
use crate::{EvaluationError, NodeError};
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::Instant;

pub struct DecisionGraph<'a, T: DecisionLoader> {
    nodes: Vec<DecisionGraphNode<'a>>,
    loader: Arc<T>,
    trace: bool,
    max_depth: u8,
    iteration: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphResponse {
    pub performance: String,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<HashMap<String, DecisionGraphTrace>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphTrace {
    input: Value,
    output: Value,
    name: String,
    id: String,
    performance: Option<String>,
    trace_data: Option<Value>,
}

pub struct DecisionGraphConfig<'a, T: DecisionLoader> {
    pub loader: Arc<T>,
    pub content: &'a DecisionContent,
    pub trace: bool,
    pub iteration: u8,
    pub max_depth: u8,
}

impl<'a, T: DecisionLoader> DecisionGraph<'a, T> {
    pub fn new(config: DecisionGraphConfig<'a, T>) -> Self {
        let nodes = config
            .content
            .nodes
            .iter()
            .map(|node| {
                let parents: Vec<&'a str> = config
                    .content
                    .edges
                    .iter()
                    .filter(|edge| edge.target_id == node.id)
                    .map(|edge| edge.source_id.as_str())
                    .collect();

                DecisionGraphNode { parents, node }
            })
            .collect();

        Self {
            nodes,
            max_depth: config.max_depth,
            iteration: config.iteration,
            trace: config.trace,
            loader: config.loader,
        }
    }

    pub async fn evaluate(&self, state: &Value) -> Result<DecisionGraphResponse, NodeError> {
        if self.iteration >= self.max_depth {
            return Err(NodeError {
                node_id: "".to_string(),
                source: anyhow!(EvaluationError::DepthLimitExceeded),
            });
        }

        let root_start = Instant::now();
        let mut node_data = HashMap::<&str, Value>::default();
        let mut node_traces = self.trace.then(|| HashMap::default());

        for graph_node in &self.nodes {
            let node = graph_node.node;
            let start = Instant::now();

            macro_rules! trace {
                ($data: tt) => {
                    if let Some(nt) = &mut node_traces {
                        nt.insert(node.id.clone(), DecisionGraphTrace $data);
                    };
                };
            }

            match node.kind {
                DecisionNodeKind::InputNode => {
                    node_data.insert(&node.id, state.clone());
                    trace!({
                        input: Value::Null,
                        output: Value::Null,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: None,
                        trace_data: None,
                    });
                }
                DecisionNodeKind::OutputNode => {
                    trace!({
                        input: Value::Null,
                        output: Value::Null,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: None,
                        trace_data: None,
                    });

                    return Ok(DecisionGraphResponse {
                        result: graph_node.parent_data(&node_data)?,
                        performance: format!("{:?}", root_start.elapsed()),
                        trace: node_traces,
                    });
                }
                DecisionNodeKind::FunctionNode { .. } => {
                    let input = graph_node.parent_data(&node_data)?;
                    let req = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input,
                    };

                    let res = FunctionHandler::new(self.trace)
                        .handle(&req)
                        .await
                        .map_err(|e| NodeError {
                            source: e.into(),
                            node_id: node.id.clone(),
                        })?;

                    node_data.insert(&node.id, res.output.clone());
                    trace!({
                        input: req.input,
                        output: res.output,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                }
                DecisionNodeKind::DecisionNode { .. } => {
                    let input = graph_node.parent_data(&node_data)?;

                    let req = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input,
                    };

                    let res = DecisionHandler::new(self.trace, self.max_depth, self.loader.clone())
                        .handle(&req)
                        .await
                        .map_err(|e| NodeError {
                            source: e.into(),
                            node_id: node.id.to_string(),
                        })?;

                    node_data.insert(&node.id, res.output.clone());
                    trace!({
                        input: req.input,
                        output: res.output,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                }
                DecisionNodeKind::DecisionTableNode { .. } => {
                    let input = graph_node.parent_data(&node_data)?;

                    let req = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input,
                    };

                    let res = DecisionTableHandler::new(self.trace)
                        .handle(&req)
                        .await
                        .map_err(|e| NodeError {
                            node_id: node.id.clone(),
                            source: e.into(),
                        })?;

                    node_data.insert(&node.id, res.output.clone());
                    trace!({
                        input: req.input,
                        output: res.output,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                }
                DecisionNodeKind::ExpressionNode { .. } => {
                    let input = graph_node.parent_data(&node_data)?;

                    let req = NodeRequest {
                        node,
                        iteration: self.iteration,
                        input,
                    };

                    let res = ExpressionHandler::new(self.trace)
                        .handle(&req)
                        .await
                        .map_err(|e| NodeError {
                            node_id: node.id.clone(),
                            source: e.into(),
                        })?;

                    node_data.insert(&node.id, res.output.clone());
                    trace!({
                        input: req.input,
                        output: res.output,
                        name: node.name.clone(),
                        id: node.id.clone(),
                        performance: Some(format!("{:?}", start.elapsed())),
                        trace_data: res.trace_data,
                    });
                }
            }
        }

        Err(NodeError {
            node_id: "".to_string(),
            source: anyhow!("Graph did not halt. Missing output node."),
        })
    }
}

struct DecisionGraphNode<'a> {
    parents: Vec<&'a str>,
    node: &'a DecisionNode,
}

impl<'a> DecisionGraphNode<'a> {
    pub fn parent_data(&self, node_data: &HashMap<&str, Value>) -> Result<Value, NodeError> {
        let mut object = Value::Object(Map::new());

        for pid in &self.parents {
            let data = node_data.get(pid).ok_or_else(|| NodeError {
                node_id: self.node.id.clone(),
                source: anyhow!("Failed to parse node data"),
            })?;

            merge_json(&mut object, data, true);
        }

        Ok(object)
    }
}

fn merge_json(doc: &mut Value, patch: &Value, top_level: bool) {
    if !patch.is_object() && !patch.is_array() && top_level {
        return;
    }

    if doc.is_object() && patch.is_object() {
        let map = doc.as_object_mut().unwrap();
        for (key, value) in patch.as_object().unwrap() {
            if value.is_null() {
                map.remove(key.as_str());
            } else {
                merge_json(map.entry(key.as_str()).or_insert(Value::Null), value, false);
            }
        }
    } else if doc.is_array() && patch.is_array() {
        let arr = doc.as_array_mut().unwrap();
        arr.extend(patch.as_array().unwrap().clone());
    } else {
        *doc = patch.clone();
    }
}

#[cfg(test)]
mod tests {
    use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
    use crate::loader::MemoryLoader;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn decision_table() {
        let content =
            &serde_json::from_str(include_str!("../../../../test-data/table.json")).unwrap();
        let tree = DecisionGraph::new(DecisionGraphConfig {
            max_depth: 5,
            trace: false,
            iteration: 0,
            content,
            loader: Arc::new(MemoryLoader::default()),
        });

        let result = tree.evaluate(&json!({ "input": 15 })).await.unwrap();

        assert_eq!(result.result, json!({ "output": 10 }));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn function() {
        let content =
            &serde_json::from_str(include_str!("../../../../test-data/function.json")).unwrap();
        let tree = DecisionGraph::new(DecisionGraphConfig {
            max_depth: 5,
            trace: false,
            iteration: 0,
            content,
            loader: Arc::new(MemoryLoader::default()),
        });

        let result = tree.evaluate(&json!({ "input": 15 })).await.unwrap();

        assert_eq!(result.result, json!({ "output": 30 }));
    }
}

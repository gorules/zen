use crate::handler::decision::DecisionHandler;
use crate::handler::function::FunctionHandler;
use crate::handler::node::{NodeError, NodeRequest};
use crate::handler::table::zen::DecisionTableHandler;
use crate::loader::DecisionLoader;
use crate::model::{DecisionContent, DecisionNode, DecisionNodeKind};
use crate::EvaluationError;
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug)]
pub struct GraphTreeNode<'a> {
    pub parents: Vec<&'a str>,
    pub node: &'a DecisionNode,
    pub data: Value,
    pub trace: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphTrace {
    input: Value,
    output: Value,
    name: String,
    id: String,
    performance: Option<String>,
    trace_data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphResponse {
    pub performance: String,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<HashMap<String, GraphTrace>>,
}

impl<'a> From<&'a DecisionNode> for GraphTreeNode<'a> {
    fn from(node: &'a DecisionNode) -> Self {
        Self {
            node,
            parents: Default::default(),
            data: Value::Null,
            trace: Value::Null,
        }
    }
}

pub struct GraphTree<'a, T: DecisionLoader> {
    nodes: RefCell<HashMap<&'a str, RefCell<GraphTreeNode<'a>>>>,
    node_ids: RefCell<Vec<&'a str>>,
    node_data: RefCell<HashMap<&'a str, Value>>,
    node_trace: RefCell<HashMap<String, GraphTrace>>,
    content: &'a DecisionContent,
    trace: bool,
    max_depth: u8,
    loader: Arc<T>,
    pub iteration: u8,
}

pub struct GraphTreeConfig<'a, T: DecisionLoader> {
    pub loader: Arc<T>,
    pub content: &'a DecisionContent,
    pub trace: bool,
    pub iteration: u8,
    pub max_depth: u8,
}

impl<'a, T: DecisionLoader> GraphTree<'a, T> {
    pub fn new(config: GraphTreeConfig<'a, T>) -> Self {
        Self {
            nodes: Default::default(),
            node_ids: Default::default(),
            node_data: Default::default(),
            node_trace: Default::default(),
            max_depth: config.max_depth,
            iteration: config.iteration,
            trace: config.trace,
            content: config.content,
            loader: config.loader,
        }
    }

    pub fn connect(&self) -> Result<(), EvaluationError> {
        if self.iteration >= self.max_depth {
            return Err(EvaluationError::DepthLimitExceeded);
        }

        let mut node_ids = self.node_ids.borrow_mut();
        let mut nodes = self.nodes.borrow_mut();

        self.content.nodes.iter().for_each(|node| {
            let key = node.id.as_str();
            node_ids.push(key);
            nodes.insert(key, RefCell::new(GraphTreeNode::from(node)));
        });

        self.content
            .edges
            .iter()
            .try_for_each::<_, Result<(), EvaluationError>>(|edge| {
                let source_ref = nodes
                    .get(edge.source_id.as_str())
                    .ok_or_else(|| EvaluationError::NodeConnectError(edge.source_id.to_string()))?;

                let target_ref = nodes
                    .get(edge.target_id.as_str())
                    .ok_or_else(|| EvaluationError::NodeConnectError(edge.target_id.to_string()))?;

                let source = source_ref.borrow();
                let mut target = target_ref.borrow_mut();

                target.parents.push(source.node.id.as_str());

                Ok(())
            })?;

        Ok(())
    }

    fn merge(doc: &mut Value, patch: &Value, top_level: bool) {
        if !patch.is_object() && !patch.is_array() && top_level {
            return;
        }

        if doc.is_object() && patch.is_object() {
            let map = doc.as_object_mut().unwrap();
            for (key, value) in patch.as_object().unwrap() {
                if value.is_null() {
                    map.remove(key.as_str());
                } else {
                    Self::merge(map.entry(key.as_str()).or_insert(Value::Null), value, false);
                }
            }
        } else if doc.is_array() && patch.is_array() {
            let arr = doc.as_array_mut().unwrap();
            arr.extend(patch.as_array().unwrap().clone());
        } else {
            *doc = patch.clone();
        }
    }

    fn parent_data(
        &self,
        node: &GraphTreeNode,
        node_data: &HashMap<&'a str, Value>,
    ) -> anyhow::Result<Value> {
        let mut object = Value::Object(Map::new());

        for pid in &node.parents {
            let data = node_data
                .get(pid)
                .ok_or_else(|| anyhow!("Failed to parse node data"))?;

            Self::merge(&mut object, data, true);
        }

        Ok(object)
    }

    fn trace(&self, key: String, trace: GraphTrace) {
        if !self.trace {
            return;
        }

        let mut node_trace = self.node_trace.borrow_mut();
        node_trace.insert(key, trace);
    }

    pub async fn evaluate(&self, state: &Value) -> Result<GraphResponse, NodeError> {
        let initial_start = Instant::now();
        let node_ids = self.node_ids.borrow();
        let nodes = self.nodes.borrow();
        let mut node_data = self.node_data.borrow_mut();

        for id in node_ids.deref() {
            let start = Instant::now();
            let node_ref = nodes.get(id).ok_or_else(|| NodeError {
                source: anyhow!("Failed to parse a reference"),
                node_id: id.to_string(),
            })?;
            let node = node_ref.borrow();

            match &node.node.kind {
                DecisionNodeKind::InputNode => {
                    node_data.insert(id, state.clone());
                    self.trace(
                        id.to_string(),
                        GraphTrace {
                            input: Value::Null,
                            output: Value::Null,
                            name: node.node.name.clone(),
                            id: node.node.id.clone(),
                            performance: None,
                            trace_data: None,
                        },
                    );
                }

                DecisionNodeKind::OutputNode => {
                    self.trace(
                        id.to_string(),
                        GraphTrace {
                            input: Value::Null,
                            output: Value::Null,
                            name: node.node.name.clone(),
                            id: node.node.id.clone(),
                            performance: None,
                            trace_data: None,
                        },
                    );

                    return Ok(GraphResponse {
                        result: self.parent_data(&node, &node_data).map_err(|e| NodeError {
                            source: e,
                            node_id: id.to_string(),
                        })?,
                        performance: format!("{:?}", initial_start.elapsed()),
                        trace: match self.trace {
                            true => Some(self.node_trace.borrow().clone()),
                            false => None,
                        },
                    });
                }
                DecisionNodeKind::FunctionNode { .. } => {
                    let input = self.parent_data(&node, &node_data).map_err(|e| NodeError {
                        node_id: id.to_string(),
                        source: e,
                    })?;
                    let req = NodeRequest {
                        node: node.node,
                        iteration: self.iteration,
                        input,
                    };

                    let res = FunctionHandler::new(self.trace)
                        .handle(&req)
                        .await
                        .map_err(|e| NodeError {
                            source: e.into(),
                            node_id: node.node.id.clone(),
                        })?;

                    node_data.insert(id, res.output.clone());
                    self.trace(
                        id.to_string(),
                        GraphTrace {
                            input: req.input,
                            output: res.output,
                            name: node.node.name.clone(),
                            id: node.node.id.clone(),
                            performance: Some(format!("{:?}", start.elapsed())),
                            trace_data: res.trace_data,
                        },
                    );
                }
                DecisionNodeKind::DecisionNode { .. } => {
                    let input = self.parent_data(&node, &node_data).map_err(|e| NodeError {
                        source: e,
                        node_id: id.to_string(),
                    })?;

                    let req = NodeRequest {
                        node: node.node,
                        iteration: self.iteration,
                        input,
                    };

                    let res = DecisionHandler::new(self.trace, self.max_depth, self.loader.clone())
                        .handle(&req)
                        .await
                        .map_err(|e| NodeError {
                            source: e.into(),
                            node_id: id.to_string(),
                        })?;

                    node_data.insert(id, res.output.clone());
                    self.trace(
                        id.to_string(),
                        GraphTrace {
                            input: req.input,
                            output: res.output,
                            name: node.node.name.clone(),
                            id: node.node.id.clone(),
                            performance: Some(format!("{:?}", start.elapsed())),
                            trace_data: res.trace_data,
                        },
                    );
                }
                DecisionNodeKind::DecisionTableNode { .. } => {
                    let input = self.parent_data(&node, &node_data).map_err(|e| NodeError {
                        source: e,
                        node_id: id.to_string(),
                    })?;

                    let req = NodeRequest {
                        node: node.node,
                        iteration: self.iteration,
                        input,
                    };

                    let res = DecisionTableHandler::new(self.trace)
                        .handle(&req)
                        .await
                        .map_err(|e| NodeError {
                            node_id: id.to_string(),
                            source: e.into(),
                        })?;

                    node_data.insert(id, res.output.clone());
                    self.trace(
                        id.to_string(),
                        GraphTrace {
                            input: req.input,
                            output: res.output,
                            name: node.node.name.clone(),
                            id: node.node.id.clone(),
                            performance: Some(format!("{:?}", start.elapsed())),
                            trace_data: res.trace_data,
                        },
                    );
                }
            };
        }

        Err(NodeError {
            node_id: "".to_string(),
            source: anyhow!("Graph did not halt. Missing output node."),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::handler::tree::{GraphTree, GraphTreeConfig};
    use crate::loader::MemoryLoader;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn decision_table() {
        let content =
            &serde_json::from_str(include_str!("../../../../test-data/table.json")).unwrap();
        let tree = GraphTree::new(GraphTreeConfig {
            max_depth: 5,
            trace: false,
            iteration: 0,
            content,
            loader: Arc::new(MemoryLoader::default()),
        });

        tree.connect().unwrap();
        let result =
            tokio_test::block_on(async { tree.evaluate(&json!({ "input": 15 })).await.unwrap() });

        assert_eq!(result.result, json!({ "output": 10 }));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn function() {
        let content =
            &serde_json::from_str(include_str!("../../../../test-data/function.json")).unwrap();
        let tree = GraphTree::new(GraphTreeConfig {
            max_depth: 5,
            trace: false,
            iteration: 0,
            content,
            loader: Arc::new(MemoryLoader::default()),
        });

        tree.connect().unwrap();
        let result =
            tokio_test::block_on(async { tree.evaluate(&json!({ "input": 15 })).await.unwrap() });

        assert_eq!(result.result, json!({ "output": 30 }));
    }
}

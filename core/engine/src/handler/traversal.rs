use std::collections::HashMap;
use std::sync::atomic::Ordering;

use fixedbitset::FixedBitSet;
use petgraph::data::DataMap;
use petgraph::matrix_graph::Zero;
use petgraph::prelude::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoNeighbors, IntoNodeIdentifiers, Reversed, VisitMap, Visitable};
use petgraph::{Incoming, Outgoing};
use serde_json::{json, Map, Value};

use crate::config::ZEN_CONFIG;
use zen_expression::Isolate;

use crate::model::{
    DecisionEdge, DecisionNode, DecisionNodeKind, SwitchStatement, SwitchStatementHitPolicy,
};

pub(crate) type StableDiDecisionGraph<'a> = StableDiGraph<&'a DecisionNode, &'a DecisionEdge>;

pub(crate) struct GraphWalker {
    ordered: FixedBitSet,
    to_visit: Vec<NodeIndex>,
    node_data: HashMap<NodeIndex, Value>,
    iter: usize,
    visited_switch_nodes: Vec<NodeIndex>,

    nodes_in_context: bool,
}

const ITER_MAX: usize = 1_000;

impl GraphWalker {
    pub fn new(graph: &StableDiDecisionGraph) -> Self {
        let mut topo = Self::empty(graph);
        topo.extend_with_initials(graph);
        topo
    }

    fn extend_with_initials(&mut self, g: &StableDiDecisionGraph) {
        // find all initial nodes (nodes without incoming edges)
        self.to_visit
            .extend(g.node_identifiers().filter(move |&nid| {
                g.neighbors_directed(nid, Incoming).count().is_zero()
                    && !g.neighbors_directed(nid, Outgoing).count().is_zero()
            }));
    }

    fn empty(graph: &StableDiDecisionGraph) -> Self {
        Self {
            ordered: graph.visit_map(),
            to_visit: Vec::new(),
            node_data: Default::default(),
            visited_switch_nodes: Default::default(),
            iter: 0,

            nodes_in_context: ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed),
        }
    }

    pub fn reset(&mut self, g: &StableDiDecisionGraph) {
        self.ordered.clear();
        self.to_visit.clear();
        self.extend_with_initials(g);

        self.iter += 1;
    }

    pub fn get_node_data(&self, node_id: NodeIndex) -> Option<&Value> {
        self.node_data.get(&node_id)
    }

    pub fn get_all_node_data(&self, g: &StableDiDecisionGraph) -> Value {
        let node_values: Map<String, Value> = self
            .node_data
            .iter()
            .map(|(idx, value)| {
                let weight = g.node_weight(*idx).unwrap();
                (weight.name.clone(), value.clone())
            })
            .collect();

        Value::Object(node_values)
    }

    pub fn set_node_data(&mut self, node_id: NodeIndex, value: Value) {
        self.node_data.insert(node_id, value);
    }

    pub fn incoming_node_data(
        &self,
        g: &StableDiDecisionGraph,
        node_id: NodeIndex,
        with_nodes: bool,
    ) -> Value {
        let mut value = self.merge_node_data(g.neighbors_directed(node_id, Incoming));

        if self.nodes_in_context {
            if let Some(object) = with_nodes.then_some(value.as_object_mut()).flatten() {
                object.insert("$nodes".to_string(), self.get_all_node_data(g));
            }
        }

        value
    }

    pub fn merge_node_data<I>(&self, iter: I) -> Value
    where
        I: Iterator<Item = NodeIndex>,
    {
        let default_map = Value::Object(Map::new());
        iter.fold(Value::Object(Map::new()), |mut prev, curr| {
            let data = self.node_data.get(&curr).unwrap_or(&default_map);

            merge_json(&mut prev, data, true);
            prev
        })
    }

    pub fn next(&mut self, g: &mut StableDiDecisionGraph) -> Option<(NodeIndex, Value)> {
        if self.iter >= ITER_MAX {
            return None;
        }
        // Take an unvisited element and find which of its neighbors are next
        let mut value = Value::Null;
        while let Some(nid) = self.to_visit.pop() {
            let decision_node = *g.node_weight(nid)?;
            if self.ordered.is_visited(&nid) {
                continue;
            }

            self.ordered.visit(nid);

            if let DecisionNodeKind::SwitchNode { content } = &decision_node.kind {
                if !self.visited_switch_nodes.contains(&nid) {
                    let mut input_data = self.incoming_node_data(g, nid, true);
                    let input_context = json!({ "$": &input_data });
                    merge_json(&mut input_data, &input_context, true);

                    let mut isolate = Isolate::with_environment(&input_data);

                    let mut statement_iter = content.statements.iter();
                    let valid_statements: Vec<&SwitchStatement> = match content.hit_policy {
                        SwitchStatementHitPolicy::First => statement_iter
                            .find(|&s| switch_statement_evaluate(&mut isolate, &s))
                            .into_iter()
                            .collect(),
                        SwitchStatementHitPolicy::Collect => statement_iter
                            .filter(|&s| switch_statement_evaluate(&mut isolate, &s))
                            .collect(),
                    };

                    let valid_statements_trace: Value = valid_statements
                        .iter()
                        .map(|&statement| json!({ "id": &statement.id }))
                        .collect();
                    value = json!({ "statements": valid_statements_trace });

                    // Remove all non-valid edges
                    let edges_to_remove: Vec<EdgeIndex> = g
                        .edges_directed(nid, Outgoing)
                        .filter(|edge| {
                            edge.weight().source_handle.as_ref().map_or(true, |handle| {
                                !valid_statements.iter().any(|s| s.id == *handle)
                            })
                        })
                        .map(|edge| edge.id())
                        .collect();
                    let edges_remove_count = edges_to_remove.len();
                    for edge in edges_to_remove {
                        remove_edge_recursive(g, edge);
                    }

                    self.visited_switch_nodes.push(nid);
                    // Reset the graph if whole branch has been removed
                    if edges_remove_count > 0 {
                        self.reset(g);
                        continue;
                    }
                }
            }

            for neigh in g.neighbors(nid) {
                // Look at each neighbor, and those that only have incoming edges
                // from the already ordered list, they are the next to visit.
                if Reversed(&*g)
                    .neighbors(neigh)
                    .all(|b| self.ordered.is_visited(&b))
                {
                    self.to_visit.push(neigh);
                }
            }

            return Some((nid, value));
        }

        None
    }
}

fn switch_statement_evaluate<'a>(
    isolate: &mut Isolate<'a>,
    switch_statement: &'a SwitchStatement,
) -> bool {
    if switch_statement.condition.is_empty() {
        return true;
    }

    isolate
        .run_standard(switch_statement.condition.as_str())
        .map_or(false, |v| v.as_bool().unwrap_or(false))
}

fn remove_edge_recursive(g: &mut StableDiDecisionGraph, edge_id: EdgeIndex) {
    let Some((source_nid, target_nid)) = g.edge_endpoints(edge_id) else {
        return;
    };

    g.remove_edge(edge_id);

    // Remove dead branches from target
    let target_incoming_count = g.edges_directed(target_nid, Incoming).count();
    if target_incoming_count.is_zero() {
        let edge_ids: Vec<EdgeIndex> = g
            .edges_directed(target_nid, Outgoing)
            .map(|edge| edge.id())
            .collect();

        edge_ids.iter().for_each(|edge_id| {
            remove_edge_recursive(g, edge_id.clone());
        });

        if g.edges(target_nid).count().is_zero() {
            g.remove_node(target_nid);
        }
    }

    // Remove dead branches from source
    let source_outgoing_count = g.edges_directed(source_nid, Outgoing).count();
    if source_outgoing_count.is_zero() {
        let edge_ids: Vec<EdgeIndex> = g
            .edges_directed(source_nid, Incoming)
            .map(|edge| edge.id())
            .collect();

        edge_ids.iter().for_each(|edge_id| {
            remove_edge_recursive(g, edge_id.clone());
        });

        if g.edges(source_nid).count().is_zero() {
            g.remove_node(source_nid);
        }
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

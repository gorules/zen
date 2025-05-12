use ahash::HashMap;
use fixedbitset::FixedBitSet;
use petgraph::data::DataMap;
use petgraph::matrix_graph::Zero;
use petgraph::prelude::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoNodeIdentifiers, VisitMap, Visitable};
use petgraph::{Incoming, Outgoing};
use serde_json::json;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use crate::config::ZEN_CONFIG;
use crate::model::{
    DecisionEdge, DecisionNode, DecisionNodeKind, SwitchStatement, SwitchStatementHitPolicy,
};
use crate::DecisionGraphTrace;
use zen_expression::variable::Variable;
use zen_expression::Isolate;

pub(crate) type StableDiDecisionGraph = StableDiGraph<Arc<DecisionNode>, Arc<DecisionEdge>>;

pub(crate) struct GraphWalker {
    ordered: FixedBitSet,
    to_visit: Vec<NodeIndex>,
    node_data: HashMap<NodeIndex, Variable>,
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
                g.node_weight(nid)
                    .is_some_and(|n| matches!(n.kind, DecisionNodeKind::InputNode { content: _ }))
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

    pub fn get_node_data(&self, node_id: NodeIndex) -> Option<Variable> {
        self.node_data.get(&node_id).cloned()
    }

    pub fn ending_variables(&self, g: &StableDiDecisionGraph) -> Variable {
        g.node_indices()
            .filter(|nid| {
                self.ordered.is_visited(nid)
                    && g.neighbors_directed(*nid, Outgoing).count().is_zero()
            })
            .fold(Variable::empty_object(), |mut acc, curr| {
                match self.node_data.get(&curr) {
                    None => acc,
                    Some(data) => acc.merge(data),
                }
            })
    }

    pub fn get_all_node_data(&self, g: &StableDiDecisionGraph) -> Variable {
        let node_values = self
            .node_data
            .iter()
            .filter_map(|(idx, value)| {
                let weight = g.node_weight(*idx)?;
                Some((Rc::from(weight.name.as_str()), value.clone()))
            })
            .collect();

        Variable::from_object(node_values)
    }

    pub fn set_node_data(&mut self, node_id: NodeIndex, value: Variable) {
        self.node_data.insert(node_id, value);
    }

    pub fn incoming_node_data(
        &self,
        g: &StableDiDecisionGraph,
        node_id: NodeIndex,
        with_nodes: bool,
    ) -> Variable {
        let value = self
            .merge_node_data(g.neighbors_directed(node_id, Incoming))
            .depth_clone(1);
        if self.nodes_in_context {
            if let Some(object_ref) = with_nodes.then_some(value.as_object()).flatten() {
                let mut object = object_ref.borrow_mut();
                object.insert(Rc::from("$nodes"), self.get_all_node_data(g));
            }
        }

        value
    }

    pub fn merge_node_data<I>(&self, iter: I) -> Variable
    where
        I: Iterator<Item = NodeIndex>,
    {
        let default_map = Variable::empty_object();
        iter.fold(Variable::empty_object(), |mut prev, curr| {
            let data = self.node_data.get(&curr).unwrap_or(&default_map);
            prev.merge_clone(data)
        })
    }

    pub fn next<F: FnMut(DecisionGraphTrace)>(
        &mut self,
        g: &mut StableDiDecisionGraph,
        mut on_trace: Option<F>,
    ) -> Option<NodeIndex> {
        let start = Instant::now();
        if self.iter >= ITER_MAX {
            return None;
        }
        // Take an unvisited element and find which of its neighbors are next
        while let Some(nid) = self.to_visit.pop() {
            let decision_node = g.node_weight(nid)?.clone();
            if self.ordered.is_visited(&nid) {
                continue;
            }

            if !self.all_dependencies_resolved(g, nid) {
                self.to_visit.push(nid);
                self.to_visit
                    .extend(self.get_unresolved_dependencies(g, nid));
                continue;
            }

            self.ordered.visit(nid);

            if let DecisionNodeKind::SwitchNode { content } = &decision_node.kind {
                if !self.visited_switch_nodes.contains(&nid) {
                    let input_data = self.incoming_node_data(g, nid, true);

                    let env = input_data.depth_clone(1);
                    env.dot_insert("$", input_data.depth_clone(1));

                    let mut isolate = Isolate::with_environment(env);

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

                    let valid_statements_trace = Variable::from_array(
                        valid_statements
                            .iter()
                            .map(|&statement| {
                                let v = Variable::empty_object();
                                v.dot_insert(
                                    "id",
                                    Variable::String(Rc::from(statement.id.as_str())),
                                );

                                v
                            })
                            .collect(),
                    );

                    input_data.dot_remove("$nodes");

                    if let Some(on_trace) = &mut on_trace {
                        on_trace(DecisionGraphTrace {
                            id: decision_node.id.clone(),
                            name: decision_node.name.clone(),
                            input: input_data.shallow_clone(),
                            output: input_data.shallow_clone(),
                            order: 0,
                            performance: Some(format!("{:.1?}", start.elapsed())),
                            trace_data: Some(
                                json!({ "statements": valid_statements_trace }).into(),
                            ),
                        });
                    }

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

            let successors = g.neighbors_directed(nid, Outgoing);
            self.to_visit.extend(successors);

            return Some(nid);
        }

        None
    }

    fn all_dependencies_resolved(&self, g: &StableDiDecisionGraph, nid: NodeIndex) -> bool {
        g.neighbors_directed(nid, Incoming)
            .all(|dep| self.ordered.is_visited(&dep))
    }

    fn get_unresolved_dependencies(
        &self,
        g: &StableDiDecisionGraph,
        nid: NodeIndex,
    ) -> Vec<NodeIndex> {
        g.neighbors_directed(nid, Incoming)
            .filter(|dep| !self.ordered.is_visited(dep))
            .collect()
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

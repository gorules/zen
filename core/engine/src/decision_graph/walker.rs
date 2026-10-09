use ahash::HashMap;
use fixedbitset::FixedBitSet;
use petgraph::data::DataMap;
use petgraph::matrix_graph::Zero;
use petgraph::prelude::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoNodeIdentifiers, VisitMap, Visitable};
use petgraph::{Incoming, Outgoing};
use std::ops::Deref;
#[cfg(feature = "driver")]
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[cfg(feature = "driver")]
use crate::config::ZEN_CONFIG;
use crate::model::{DecisionEdge, DecisionNode, DecisionNodeKind, SwitchNodeContent};
use zen_expression::variable::Variable;

pub(crate) type StableDiDecisionGraph = StableDiGraph<Arc<DecisionNode>, Arc<DecisionEdge>>;

pub(crate) struct NodeData {
    #[cfg(feature = "driver")]
    pub name: zen_types::symbol::Symbol,
    #[cfg(feature = "driver")]
    pub data: Variable,
    #[cfg(feature = "driver")]
    pub nodes_view: Option<Variable>,
}

impl NodeData {
    #[cfg_attr(not(feature = "driver"), allow(unused_variables))]
    pub(crate) fn placeholder(name: &str) -> Self {
        Self {
            #[cfg(feature = "driver")]
            name: zen_types::symbol::Symbol::from(name),
            #[cfg(feature = "driver")]
            data: Variable::Null,
            #[cfg(feature = "driver")]
            nodes_view: None,
        }
    }
}

pub(crate) enum WalkStep {
    Node(NodeIndex),
    Pending,
    Done,
}

pub(crate) struct GraphWalker {
    iter: usize,
    pub(super) node_data: HashMap<NodeIndex, NodeData>,
    ordered: FixedBitSet,
    to_visit: Vec<NodeIndex>,
    visited_switch_nodes: Vec<NodeIndex>,

    #[cfg(feature = "driver")]
    pub(super) nodes_in_context: bool,
}

const ITER_MAX: usize = 1_000;

impl GraphWalker {
    pub fn new(graph: &StableDiDecisionGraph) -> Self {
        let mut walker = Self::empty(graph);
        walker.initialize_input_nodes(graph);
        walker
    }

    fn initialize_input_nodes(&mut self, g: &StableDiDecisionGraph) {
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

            #[cfg(feature = "driver")]
            nodes_in_context: ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed),
        }
    }

    pub fn reset(&mut self, g: &StableDiDecisionGraph) {
        self.ordered.clear();
        self.to_visit.clear();
        self.initialize_input_nodes(g);

        self.iter += 1;
    }

    pub(crate) fn merge_ending<'a>(values: impl Iterator<Item = &'a Variable>) -> Variable {
        values.fold(Variable::empty_object(), |mut acc, data| acc.merge(data))
    }

    pub fn set_node_data(&mut self, node_id: NodeIndex, value: NodeData) {
        self.node_data.insert(node_id, value);
    }

    pub(crate) fn merge_values<'a>(mut incoming: impl Iterator<Item = &'a Variable>) -> Variable {
        let Some(first) = incoming.next() else {
            return Variable::empty_object();
        };

        let head = match first {
            Variable::Object(_) | Variable::Array(_) => first.clone(),
            _ => Variable::empty_object(),
        };

        incoming.fold(head, |mut prev, data| prev.merge_clone(data))
    }

    pub(crate) fn next_with<S>(&mut self, g: &mut StableDiDecisionGraph, mut switch: S) -> WalkStep
    where
        S: FnMut(
            &Self,
            &StableDiDecisionGraph,
            NodeIndex,
            &DecisionNode,
            &SwitchNodeContent,
        ) -> Option<Vec<Arc<str>>>,
    {
        if self.iter >= ITER_MAX {
            return WalkStep::Done;
        }
        // Take an unvisited element and find which of its neighbors are next
        while let Some(nid) = self.to_visit.pop() {
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

            let Some(decision_node) = g.node_weight(nid).cloned() else {
                return WalkStep::Done;
            };
            if let DecisionNodeKind::SwitchNode { content } = &decision_node.kind {
                if !self.visited_switch_nodes.contains(&nid) {
                    let Some(valid_statements) = switch(self, g, nid, &decision_node, content)
                    else {
                        return WalkStep::Pending;
                    };

                    // Remove all non-valid edges
                    let edges_to_remove: Vec<EdgeIndex> = g
                        .edges_directed(nid, Outgoing)
                        .filter(|edge| {
                            edge.weight().source_handle.as_ref().map_or(true, |handle| {
                                !valid_statements
                                    .iter()
                                    .any(|s| s.deref() == handle.deref())
                            })
                        })
                        .map(|edge| edge.id())
                        .collect();
                    let edges_remove_count = edges_to_remove.len();
                    for edge in edges_to_remove {
                        Self::remove_edge_recursive(g, edge);
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

            return WalkStep::Node(nid);
        }

        WalkStep::Done
    }

    pub(crate) fn has_node_data(&self, node_id: NodeIndex) -> bool {
        self.node_data.contains_key(&node_id)
    }

    pub(crate) fn ending_nodes(&self, g: &StableDiDecisionGraph) -> Vec<NodeIndex> {
        g.node_indices()
            .filter(|nid| {
                self.ordered.is_visited(nid)
                    && g.neighbors_directed(*nid, Outgoing).count().is_zero()
                    && self.node_data.contains_key(nid)
            })
            .collect()
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

    fn remove_edge_recursive(g: &mut StableDiDecisionGraph, edge_id: EdgeIndex) {
        let Some((source_nid, target_nid)) = g.edge_endpoints(edge_id) else {
            return;
        };

        g.remove_edge(edge_id);

        for (nid, direction) in [(target_nid, Incoming), (source_nid, Outgoing)] {
            let count = g.edges_directed(nid, direction).count();
            if count.is_zero() {
                let edge_ids: Vec<EdgeIndex> = g
                    .edges_directed(nid, direction.opposite())
                    .map(|edge| edge.id())
                    .collect();

                edge_ids.iter().for_each(|&edge_id| {
                    Self::remove_edge_recursive(g, edge_id);
                });

                if g.edges(nid).count().is_zero() {
                    g.remove_node(nid);
                }
            }
        }
    }
}

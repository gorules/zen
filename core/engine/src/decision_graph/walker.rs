use ahash::HashMap;
use fixedbitset::FixedBitSet;
use petgraph::data::DataMap;
use petgraph::matrix_graph::Zero;
use petgraph::prelude::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoNodeIdentifiers, VisitMap, Visitable};
use petgraph::{Incoming, Outgoing};
use std::ops::Deref;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use crate::config::ZEN_CONFIG;
use crate::model::{
    DecisionEdge, DecisionNode, DecisionNodeKind, SwitchStatement, SwitchStatementHitPolicy,
};
use crate::DecisionGraphTrace;
use zen_expression::variable::{ToVariable, Variable};
use zen_expression::Isolate;

pub(crate) type StableDiDecisionGraph = StableDiGraph<Arc<DecisionNode>, Arc<DecisionEdge>>;

pub(crate) struct NodeData {
    pub name: Rc<str>,
    pub data: Variable,
}

pub(crate) struct GraphWalker {
    iter: usize,
    node_data: HashMap<NodeIndex, NodeData>,
    ordered: FixedBitSet,
    to_visit: Vec<NodeIndex>,
    visited_switch_nodes: Vec<NodeIndex>,

    nodes_in_context: bool,
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

            nodes_in_context: ZEN_CONFIG.nodes_in_context.load(Ordering::Relaxed),
        }
    }

    pub fn reset(&mut self, g: &StableDiDecisionGraph) {
        self.ordered.clear();
        self.to_visit.clear();
        self.initialize_input_nodes(g);

        self.iter += 1;
    }

    pub fn get_node_data(&self, node_id: NodeIndex) -> Option<Variable> {
        Some(self.node_data.get(&node_id)?.data.clone())
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
                    Some(nd) => acc.merge(&nd.data),
                }
            })
    }

    pub fn get_all_node_data(&self) -> Variable {
        let node_values = self
            .node_data
            .iter()
            .filter_map(|(_, nd)| Some((nd.name.clone(), nd.data.clone())))
            .collect();

        Variable::from_object(node_values)
    }

    pub fn set_node_data(&mut self, node_id: NodeIndex, value: NodeData) {
        self.node_data.insert(node_id, value);
    }

    pub fn incoming_node_data(
        &self,
        g: &StableDiDecisionGraph,
        node_id: NodeIndex,
        with_nodes: bool,
    ) -> (Variable, Variable) {
        let value = self.merge_node_data(g.neighbors_directed(node_id, Incoming));

        if self.nodes_in_context && with_nodes {
            if let Some(object_ref) = value.as_object() {
                let mut new_object = object_ref.borrow().clone();
                new_object.insert(Rc::from("$nodes"), self.get_all_node_data());

                return (Variable::from_object(new_object), value);
            }
        }

        (value.depth_clone(1), value)
    }

    pub fn merge_node_data<I>(&self, iter: I) -> Variable
    where
        I: Iterator<Item = NodeIndex>,
    {
        iter.filter_map(|nid| self.node_data.get(&nid))
            .fold(Variable::empty_object(), |mut prev, nd| {
                prev.merge_clone(&nd.data)
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

            let decision_node = g.node_weight(nid)?.clone();
            if let DecisionNodeKind::SwitchNode { content } = &decision_node.kind {
                if !self.visited_switch_nodes.contains(&nid) {
                    let (input, input_trace) = self.incoming_node_data(g, nid, true);
                    let mut isolate = Isolate::with_environment(input);

                    let mut statement_iter = content.statements.iter();
                    let valid_statements: Vec<SwitchStatementTraceRow> = match content.hit_policy {
                        SwitchStatementHitPolicy::First => statement_iter
                            .find(|&s| switch_statement_evaluate(&mut isolate, &s))
                            .into_iter()
                            .cloned()
                            .map(SwitchStatementTraceRow::from)
                            .collect(),
                        SwitchStatementHitPolicy::Collect => statement_iter
                            .filter(|&s| switch_statement_evaluate(&mut isolate, &s))
                            .cloned()
                            .map(SwitchStatementTraceRow::from)
                            .collect(),
                    };

                    if let Some(on_trace) = &mut on_trace {
                        on_trace(DecisionGraphTrace {
                            id: decision_node.id.clone(),
                            name: decision_node.name.clone(),
                            input: input_trace.shallow_clone(),
                            output: input_trace,
                            order: 0,
                            performance: Some(Arc::from(format!("{:.1?}", start.elapsed()))),
                            trace_data: Some(
                                SwitchStatementTrace {
                                    statements: valid_statements.clone(),
                                }
                                .to_variable(),
                            ),
                        });
                    }

                    // Remove all non-valid edges
                    let edges_to_remove: Vec<EdgeIndex> = g
                        .edges_directed(nid, Outgoing)
                        .filter(|edge| {
                            edge.weight().source_handle.as_ref().map_or(true, |handle| {
                                !valid_statements
                                    .iter()
                                    .any(|s| s.id.deref() == handle.deref())
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
        .run_standard(switch_statement.condition.deref())
        .map_or(false, |v| v.as_bool().unwrap_or(false))
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
                remove_edge_recursive(g, edge_id);
            });

            if g.edges(nid).count().is_zero() {
                g.remove_node(nid);
            }
        }
    }
}

#[derive(ToVariable)]
struct SwitchStatementTrace {
    statements: Vec<SwitchStatementTraceRow>,
}

#[derive(ToVariable, Clone)]
#[serde(rename_all = "camelCase")]
struct SwitchStatementTraceRow {
    pub id: Arc<str>,
}

impl From<SwitchStatement> for SwitchStatementTraceRow {
    fn from(value: SwitchStatement) -> Self {
        Self { id: value.id }
    }
}

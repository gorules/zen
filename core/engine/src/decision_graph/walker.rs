use ahash::HashMap;
use fixedbitset::FixedBitSet;
use petgraph::data::DataMap;
use petgraph::matrix_graph::Zero;
use petgraph::prelude::{EdgeIndex, NodeIndex, StableDiGraph};
use petgraph::visit::{EdgeRef, IntoNodeIdentifiers, VisitMap, Visitable};
use petgraph::{Incoming, Outgoing};
use std::ops::Deref;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use crate::config::ZEN_CONFIG;
use crate::model::{
    DecisionEdge, DecisionNode, DecisionNodeKind, SwitchNodeContent, SwitchStatement,
    SwitchStatementHitPolicy,
};
use crate::DecisionGraphTrace;
use zen_expression::variable::{ToVariable, Variable};
use zen_expression::Isolate;

pub(crate) type StableDiDecisionGraph = StableDiGraph<Arc<DecisionNode>, Arc<DecisionEdge>>;

pub(crate) struct NodeData {
    pub name: zen_types::symbol::Symbol,
    pub data: Variable,
    pub nodes_view: Option<Variable>,
}

pub(crate) enum WalkStep {
    Node(NodeIndex),
    Pending,
    Done,
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
        Self::merge_ending(
            self.ending_nodes(g)
                .into_iter()
                .filter_map(|nid| self.node_data.get(&nid).map(|nd| &nd.data)),
        )
    }

    pub(crate) fn merge_ending<'a>(values: impl Iterator<Item = &'a Variable>) -> Variable {
        values.fold(Variable::empty_object(), |mut acc, data| acc.merge(data))
    }

    pub fn get_all_node_data(&self) -> Variable {
        let node_values = self
            .node_data
            .iter()
            .filter_map(|(_, nd)| {
                let value = nd.nodes_view.clone().unwrap_or_else(|| nd.data.clone());
                Some((nd.name.clone(), value))
            })
            .collect();

        Variable::from_object(node_values)
    }

    pub fn set_node_data(&mut self, node_id: NodeIndex, value: NodeData) {
        self.node_data.insert(node_id, value);
    }

    pub fn nodes_context(&self) -> Option<Variable> {
        self.nodes_in_context.then(|| self.get_all_node_data())
    }

    pub fn incoming_node_data(
        &self,
        g: &StableDiDecisionGraph,
        node_id: NodeIndex,
    ) -> (Variable, Variable) {
        let value = self.merge_node_data(g.neighbors_directed(node_id, Incoming));

        (value.clone(), value)
    }

    pub fn merge_node_data<I>(&self, iter: I) -> Variable
    where
        I: Iterator<Item = NodeIndex>,
    {
        Self::merge_values(iter.filter_map(|nid| self.node_data.get(&nid).map(|nd| &nd.data)))
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

    pub fn next<F: FnMut(DecisionGraphTrace)>(
        &mut self,
        g: &mut StableDiDecisionGraph,
        mut on_trace: Option<F>,
    ) -> Option<NodeIndex> {
        let start = Instant::now();
        let step = self.next_with(g, |walker, g, nid, node, content| {
            let (input, input_trace) = walker.incoming_node_data(g, nid);
            let mut isolate = Isolate::with_environment(input);
            if let Some(nodes) = walker.nodes_context() {
                isolate.set_local(Variable::nodes_key(), nodes);
            }

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
                let output = input_trace.depth_clone(1);

                on_trace(DecisionGraphTrace {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    input: input_trace,
                    output,
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

            Some(valid_statements.into_iter().map(|s| s.id).collect())
        });

        match step {
            WalkStep::Node(nid) => Some(nid),
            WalkStep::Done | WalkStep::Pending => None,
        }
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
}

fn switch_statement_evaluate(isolate: &mut Isolate, switch_statement: &SwitchStatement) -> bool {
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

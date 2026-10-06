use crate::decision_graph::walker::{GraphWalker, NodeData, StableDiDecisionGraph, WalkStep};
use crate::model::DecisionNodeKind;
use petgraph::prelude::NodeIndex;
use petgraph::Incoming;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use zen_expression::variable::Variable;

pub(crate) type Outcome = Arc<[Arc<str>]>;

type Pending = (NodeIndex, Box<[NodeIndex]>, Box<[NodeIndex]>);

pub(crate) struct Execute {
    pub node: NodeIndex,
    pub parents: Box<[NodeIndex]>,
    pub visible: Box<[NodeIndex]>,
}

pub(crate) enum End {
    Finish(Box<[NodeIndex]>),
    Switch {
        node: NodeIndex,
        parents: Box<[NodeIndex]>,
        visible: Box<[NodeIndex]>,
        children: RwLock<HashMap<Outcome, Arc<Segment>>>,
    },
}

pub(crate) struct Segment {
    pub path: Box<[Outcome]>,
    pub events: Box<[Execute]>,
    pub end: End,
}

pub(crate) struct Replay<'a> {
    graph: &'a StableDiDecisionGraph,
}

impl<'a> Replay<'a> {
    pub fn new(graph: &'a StableDiDecisionGraph) -> Self {
        Self { graph }
    }

    pub fn segment(&self, path: &[Outcome]) -> Segment {
        let mut graph = self.graph.clone();
        let mut walker = GraphWalker::new(&graph);
        let mut executed: Vec<NodeIndex> = Vec::new();
        let mut events: Vec<Execute> = Vec::new();
        let mut consumed = 0usize;
        let mut pending: Option<Pending> = None;

        loop {
            let step = walker.next_with(&mut graph, |_, g, nid, _, _| {
                let outcome = path.get(consumed).cloned();
                match outcome {
                    Some(outcome) => {
                        consumed += 1;
                        Some(outcome.iter().cloned().collect())
                    }
                    None => {
                        pending = Some((
                            nid,
                            g.neighbors_directed(nid, Incoming).collect(),
                            executed.clone().into_boxed_slice(),
                        ));
                        None
                    }
                }
            });
            let nid = match step {
                WalkStep::Node(nid) => nid,
                WalkStep::Pending => {
                    let (node, parents, visible) = pending.take().unwrap_or_default();
                    return Segment {
                        path: path.into(),
                        events: events.into_boxed_slice(),
                        end: End::Switch {
                            node,
                            parents,
                            visible,
                            children: RwLock::new(HashMap::new()),
                        },
                    };
                }
                WalkStep::Done => break,
            };
            if walker.has_node_data(nid) {
                continue;
            }
            let Some(node) = graph.node_weight(nid).cloned() else {
                break;
            };
            if consumed == path.len() {
                events.push(Execute {
                    node: nid,
                    parents: graph.neighbors_directed(nid, Incoming).collect(),
                    visible: executed.clone().into_boxed_slice(),
                });
            }
            executed.push(nid);
            walker.set_node_data(
                nid,
                NodeData {
                    name: zen_types::symbol::Symbol::from(node.name.as_ref()),
                    data: Variable::Null,
                    nodes_view: None,
                },
            );
            if matches!(node.kind, DecisionNodeKind::OutputNode { .. }) {
                break;
            }
        }

        Segment {
            path: path.into(),
            events: events.into_boxed_slice(),
            end: End::Finish(walker.ending_nodes(&graph).into_boxed_slice()),
        }
    }
}

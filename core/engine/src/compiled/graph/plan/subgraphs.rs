use crate::compiled::graph::call::Call;
use crate::compiled::graph::CompiledGraph;
use crate::model::{DecisionNodeKind, GraphContent};
use crate::nodes::NodeHandlerExtensions;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct Subgraphs {
    graphs: Vec<(Arc<str>, Arc<GraphContent>)>,
}

pub(crate) type Closure = Vec<(Arc<str>, Arc<GraphContent>)>;

impl Subgraphs {
    pub async fn resolve(graph: &CompiledGraph, extensions: &NodeHandlerExtensions) -> Subgraphs {
        let mut found = Subgraphs::default();
        let mut pending: Vec<Arc<str>> = Self::keys(graph);
        let mut seen: Vec<Arc<str>> = Vec::new();
        while let Some(key) = pending.pop() {
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            let Ok(content) = CompiledGraph::resolve_key(&key, extensions).await else {
                continue;
            };
            if let Ok(child) = Call::compiled(&content) {
                pending.extend(Self::keys(child));
            }
            found.graphs.push((key, content));
        }
        found
    }

    pub fn wanted(graph: &CompiledGraph) -> bool {
        graph
            .steps
            .iter()
            .flatten()
            .any(|step| matches!(step.node.kind, DecisionNodeKind::DecisionNode { .. }))
    }

    fn keys(graph: &CompiledGraph) -> Vec<Arc<str>> {
        graph
            .steps
            .iter()
            .flatten()
            .filter_map(|step| match &step.node.kind {
                DecisionNodeKind::DecisionNode { content } => Some(content.key.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn get(&self, key: &str) -> Option<&Arc<GraphContent>> {
        self.graphs
            .iter()
            .find(|(k, _)| k.as_ref() == key)
            .map(|(_, content)| content)
    }

    pub fn closure(&self, graph: &CompiledGraph) -> Closure {
        let mut out: Closure = Vec::new();
        let mut pending = Self::keys(graph);
        while let Some(key) = pending.pop() {
            if out.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let Some(content) = self.get(&key) else {
                continue;
            };
            if let Ok(child) = Call::compiled(content) {
                pending.extend(Self::keys(child));
            }
            out.push((key, content.clone()));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn same(a: &Closure, b: &Closure) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|((ka, ca), (kb, cb))| ka == kb && Arc::ptr_eq(ca, cb))
    }
}

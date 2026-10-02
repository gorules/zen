use std::collections::VecDeque;
use std::sync::Arc;

use ahash::{HashMap, HashSet};
use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};
use zen_types::decision::DecisionNodeKind;

use crate::model::DecisionContent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependencyKind {
    Import,
    Dictionaries,
    Signature,
}

pub type Dependency = (Arc<str>, DependencyKind);

#[derive(Default)]
pub struct DependencyIndex {
    uses: HashMap<Arc<str>, Vec<Dependency>>,
    used_by: HashMap<Arc<str>, HashSet<Arc<str>>>,
}

impl DependencyIndex {
    pub fn set(&mut self, document: Arc<str>, content: &DecisionContent) {
        self.remove(&document);
        let uses = Self::declared(content);
        for (dependency, _) in &uses {
            self.used_by
                .entry(dependency.clone())
                .or_default()
                .insert(document.clone());
        }
        self.uses.insert(document, uses);
    }

    pub fn remove(&mut self, document: &str) {
        let Some(uses) = self.uses.remove(document) else {
            return;
        };
        for (dependency, _) in uses {
            if let Some(users) = self.used_by.get_mut(&dependency) {
                users.remove(document);
                if users.is_empty() {
                    self.used_by.remove(&dependency);
                }
            }
        }
    }

    pub fn uses(&self, document: &str) -> &[Dependency] {
        self.uses
            .get(document)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn used_by(&self, document: &str) -> Vec<Dependency> {
        let mut out: Vec<Dependency> = self
            .used_by
            .get(document)
            .into_iter()
            .flatten()
            .flat_map(|user| {
                self.uses(user)
                    .iter()
                    .filter(|(dependency, _)| dependency.as_ref() == document)
                    .map(|(_, kind)| (user.clone(), *kind))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn affected(
        &self,
        changed: &[Arc<str>],
        mut reaches: impl FnMut(&Arc<str>, &Arc<str>, DependencyKind) -> bool,
    ) -> Vec<Arc<str>> {
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        let mut queue: VecDeque<Arc<str>> = changed.iter().cloned().collect();
        while let Some(document) = queue.pop_front() {
            if !seen.insert(document.clone()) {
                continue;
            }
            for (user, kind) in self.used_by(&document) {
                if !seen.contains(&user) && reaches(&user, &document, kind) {
                    queue.push_back(user);
                }
            }
        }
        let mut out: Vec<Arc<str>> = seen.into_iter().collect();
        out.sort();
        out
    }

    pub fn signature_cycles(&self) -> HashMap<Arc<str>, usize> {
        let mut graph: DiGraph<Arc<str>, ()> = DiGraph::new();
        let mut nodes: HashMap<Arc<str>, NodeIndex> = HashMap::default();
        let mut node = |graph: &mut DiGraph<Arc<str>, ()>, path: &Arc<str>| {
            *nodes
                .entry(path.clone())
                .or_insert_with(|| graph.add_node(path.clone()))
        };
        for (user, uses) in &self.uses {
            for (dependency, kind) in uses {
                if *kind == DependencyKind::Signature {
                    let from = node(&mut graph, user);
                    let to = node(&mut graph, dependency);
                    graph.add_edge(from, to, ());
                }
            }
        }
        let mut out: HashMap<Arc<str>, usize> = HashMap::default();
        for (id, component) in tarjan_scc(&graph).into_iter().enumerate() {
            let cyclic = component.len() > 1
                || component
                    .first()
                    .is_some_and(|&n| graph.contains_edge(n, n));
            if cyclic {
                for n in component {
                    out.insert(graph[n].clone(), id);
                }
            }
        }
        out
    }

    fn declared(content: &DecisionContent) -> Vec<Dependency> {
        let mut out: Vec<Dependency> = Vec::new();
        let mut push = |path: &Arc<str>, kind: DependencyKind| {
            if !out.iter().any(|(p, k)| p == path && *k == kind) {
                out.push((path.clone(), kind));
            }
        };
        match content {
            DecisionContent::Policy(policy) => {
                for import in &policy.0.imports {
                    push(import, DependencyKind::Import);
                }
            }
            DecisionContent::Graph(graph) => {
                for import in &graph.imports {
                    push(import, DependencyKind::Dictionaries);
                }
                for node in &graph.nodes {
                    if let DecisionNodeKind::DecisionNode { content } = &node.kind {
                        push(&content.key, DependencyKind::Signature);
                    }
                }
            }
        }
        out
    }
}

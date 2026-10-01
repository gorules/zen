use std::collections::VecDeque;
use std::sync::Arc;

use ahash::{HashMap, HashSet};
use zen_types::decision::DecisionNodeKind;

use crate::workspace::db::{Db, Snapshot};
use crate::workspace::reads::ReadView;

type Edge = (Arc<str>, Option<ReadView>);

impl Db {
    pub fn affected_by(&self, paths: &[&str]) -> Vec<Arc<str>> {
        let snap = self.snapshot();
        let mut dependents: HashMap<Arc<str>, Vec<Edge>> = HashMap::default();
        for path in self.document_paths() {
            for (dependency, view) in self.direct_dependencies(&snap, &path) {
                if dependency != path {
                    dependents
                        .entry(dependency)
                        .or_default()
                        .push((path.clone(), view));
                }
            }
        }
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        let mut queue: VecDeque<Arc<str>> = paths.iter().map(|p| Arc::from(*p)).collect();
        while let Some(path) = queue.pop_front() {
            if !seen.insert(path.clone()) {
                continue;
            }
            for (dependent, view) in dependents.get(&path).into_iter().flatten() {
                if seen.contains(dependent) {
                    continue;
                }
                let reaches = match view {
                    Some(view) => !self.view_holds(&path, view),
                    None => true,
                };
                if reaches {
                    queue.push_back(dependent.clone());
                }
            }
        }
        let mut out: Vec<Arc<str>> = seen.into_iter().collect();
        out.sort();
        out
    }

    fn direct_dependencies(&self, snap: &Snapshot, path: &Arc<str>) -> Vec<Edge> {
        if let Some(parsed) = snap.all_parsed.get(path) {
            return parsed
                .policy
                .imports()
                .iter()
                .map(|import| (import.clone(), None))
                .collect();
        }
        if let Some(edges) = self.recorded_edges(path) {
            return edges;
        }
        let Some(content) = snap.graphs.get(path).and_then(|doc| doc.as_graph()) else {
            return Vec::new();
        };
        let mut out: HashSet<Arc<str>> = HashSet::default();
        out.extend(content.imports.iter().cloned());
        for node in &content.nodes {
            let DecisionNodeKind::DecisionNode { content } = &node.kind else {
                continue;
            };
            out.insert(content.key.clone());
            if let Some(&component) = snap.policy_to_component.get(&content.key) {
                out.extend(snap.components[component].iter().cloned());
            }
        }
        out.extend(self.recorded_reads(path));
        out.into_iter()
            .map(|dependency| (dependency, None))
            .collect()
    }
}

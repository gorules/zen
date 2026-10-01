use std::collections::VecDeque;
use std::sync::Arc;

use ahash::{HashMap, HashSet};
use zen_types::decision::DecisionNodeKind;

use crate::workspace::db::{Db, Snapshot};

impl Db {
    pub fn affected_by(&self, paths: &[&str]) -> Vec<Arc<str>> {
        let snap = self.snapshot();
        let mut dependents: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::default();
        for path in self.document_paths() {
            for dependency in self.direct_dependencies(&snap, &path) {
                if dependency != path {
                    dependents.entry(dependency).or_default().push(path.clone());
                }
            }
        }
        let mut seen: HashSet<Arc<str>> = HashSet::default();
        let mut queue: VecDeque<Arc<str>> = paths.iter().map(|p| Arc::from(*p)).collect();
        while let Some(path) = queue.pop_front() {
            if !seen.insert(path.clone()) {
                continue;
            }
            if let Some(next) = dependents.get(&path) {
                queue.extend(next.iter().cloned());
            }
        }
        let mut out: Vec<Arc<str>> = seen.into_iter().collect();
        out.sort();
        out
    }

    fn direct_dependencies(&self, snap: &Snapshot, path: &Arc<str>) -> HashSet<Arc<str>> {
        let mut out: HashSet<Arc<str>> = HashSet::default();
        if let Some(parsed) = snap.all_parsed.get(path) {
            out.extend(parsed.policy.imports().iter().cloned());
        }
        let Some(content) = snap.graphs.get(path).and_then(|doc| doc.as_graph()) else {
            return out;
        };
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
        out
    }
}

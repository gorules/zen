use std::sync::Arc;

use ahash::{HashMap, HashMapExt, HashSet};

use crate::policy::db::Db;
use crate::policy::types::WriteConflict;

impl Db {
    pub fn component_members(&self, policy: &str) -> Vec<Arc<str>> {
        let snap = self.snapshot();
        match snap.policy_to_component.get(policy) {
            Some(&idx) => snap.components.get(idx).cloned().unwrap_or_default(),
            None => Vec::new(),
        }
    }

    pub fn cross_component_write_conflicts(&self) -> Vec<WriteConflict> {
        let snap = self.snapshot();
        let mut by_path: HashMap<Arc<str>, Vec<(Arc<str>, usize)>> = HashMap::new();
        for rule in &snap.shallow.per_rule {
            let Some(&component) = snap.policy_to_component.get(&rule.policy_path) else {
                continue;
            };
            for write in &rule.writes {
                if write.path.is_empty() {
                    continue;
                }
                by_path
                    .entry(write.path.clone())
                    .or_default()
                    .push((rule.policy_path.clone(), component));
            }
        }

        let mut conflicts: Vec<WriteConflict> = by_path
            .into_iter()
            .filter_map(|(path, writers)| {
                let components: HashSet<usize> = writers.iter().map(|(_, c)| *c).collect();
                if components.len() < 2 {
                    return None;
                }
                let mut policies: Vec<Arc<str>> = writers.into_iter().map(|(p, _)| p).collect();
                policies.sort();
                policies.dedup();
                Some(WriteConflict { path, policies })
            })
            .collect();
        conflicts.sort_by(|a, b| a.path.cmp(&b.path));
        conflicts
    }
}

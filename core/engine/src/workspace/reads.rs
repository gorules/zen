use std::collections::VecDeque;
use std::sync::Arc;

use ahash::HashSet;

use crate::policy::ir::DictionaryIr;
use crate::workspace::db::Db;
use crate::workspace::graph::SignatureResolution;

#[derive(Clone, PartialEq)]
pub(crate) enum ReadView {
    Dictionaries(DictionaryView),
    Signature(SignatureResolution),
}

#[derive(Clone, PartialEq)]
pub(crate) enum DictionaryView {
    Missing,
    Graph,
    Policy(Vec<(Arc<str>, Arc<str>, Arc<DictionaryIr>)>),
}

impl Db {
    pub(crate) fn dictionary_view(&self, path: &Arc<str>) -> DictionaryView {
        let snap = self.snapshot();
        if !snap.all_parsed.contains_key(path) {
            return match snap.graphs.contains_key(path) {
                true => DictionaryView::Graph,
                false => DictionaryView::Missing,
            };
        }
        let mut visited: HashSet<Arc<str>> = HashSet::default();
        let mut queue: VecDeque<Arc<str>> = VecDeque::from([path.clone()]);
        let mut entries = Vec::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            let Some(parsed) = snap.all_parsed.get(&current) else {
                continue;
            };
            for block in &parsed.policy.dictionaries {
                entries.push((current.clone(), block.id.clone(), block.ir.clone()));
            }
            queue.extend(parsed.policy.imports().iter().cloned());
        }
        DictionaryView::Policy(entries)
    }

    pub(crate) fn view_holds(&self, path: &Arc<str>, view: &ReadView) -> bool {
        match view {
            ReadView::Dictionaries(recorded) => self.dictionary_view(path) == *recorded,
            ReadView::Signature(recorded) => {
                self.graph_dep_frame_push(path);
                let current = self.decision_signature(path);
                let _ = self.graph_dep_frame_pop();
                current == *recorded
            }
        }
    }
}

use std::sync::Arc;

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
    Policy {
        dictionaries: Vec<(Arc<str>, Arc<DictionaryIr>)>,
        imports: Vec<Arc<str>>,
    },
}

impl Db {
    pub(crate) fn dictionary_view(&self, path: &Arc<str>) -> DictionaryView {
        let snap = self.snapshot();
        if let Some(parsed) = snap.all_parsed.get(path) {
            return DictionaryView::Policy {
                dictionaries: parsed
                    .policy
                    .dictionaries
                    .iter()
                    .map(|block| (block.id.clone(), block.ir.clone()))
                    .collect(),
                imports: parsed.policy.imports().to_vec(),
            };
        }
        match snap.graphs.contains_key(path) {
            true => DictionaryView::Graph,
            false => DictionaryView::Missing,
        }
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

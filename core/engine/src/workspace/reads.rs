use std::sync::Arc;

use crate::policy::ir::DictionaryIr;
use crate::workspace::db::Db;
use crate::workspace::graph::{EntityUnitEntry, SignatureResolution};

#[derive(Clone, PartialEq)]
pub(crate) enum ReadView {
    Dictionaries(DictionaryView),
    Signature(SignatureResolution),
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ImportKind {
    Missing,
    Graph,
    Policy,
}

#[derive(Clone, PartialEq)]
pub(crate) struct DictionaryView {
    imports: Vec<(Arc<str>, ImportKind)>,
    entries: Vec<(Arc<str>, Arc<str>, Arc<DictionaryIr>)>,
    /// The entities seen through the same imports (a Request node typed by one).
    entities: Vec<EntityUnitEntry>,
}

impl Db {
    pub(crate) fn dictionary_view(&self, imports: &[Arc<str>]) -> DictionaryView {
        let snap = self.snapshot();
        DictionaryView {
            imports: imports
                .iter()
                .map(|import| {
                    let kind = match (
                        snap.all_parsed.contains_key(import),
                        snap.graphs.contains_key(import),
                    ) {
                        (true, _) => ImportKind::Policy,
                        (false, true) => ImportKind::Graph,
                        (false, false) => ImportKind::Missing,
                    };
                    (import.clone(), kind)
                })
                .collect(),
            entries: self
                .graph_dictionary_blocks(imports)
                .into_iter()
                .map(|entry| (entry.policy_path, entry.block_id, entry.ir))
                .collect(),
            entities: self.graph_entity_blocks(imports),
        }
    }

    pub(crate) fn view_holds(&self, path: &Arc<str>, view: &ReadView) -> bool {
        match view {
            ReadView::Dictionaries(recorded) => {
                let imports: Vec<Arc<str>> = recorded
                    .imports
                    .iter()
                    .map(|(import, _)| import.clone())
                    .collect();
                self.dictionary_view(&imports) == *recorded
            }
            ReadView::Signature(recorded) => {
                self.graph_dep_frame_push(path);
                let current = self.decision_signature(path);
                let _ = self.graph_dep_frame_pop();
                current == *recorded
            }
        }
    }
}

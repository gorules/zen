use std::sync::Arc;

use ahash::HashSet;
use zen_types::decision::DecisionNodeKind;

use crate::model::DecisionContent;
use crate::workspace::db::Db;
use crate::workspace::document_dependencies::DependencyKind;
use crate::workspace::types::PathEdit;

impl Db {
    pub(crate) fn move_paths(&self, moves: &[(Arc<str>, Arc<str>)]) -> Vec<PathEdit> {
        let mut seen: HashSet<&str> = HashSet::default();
        let mut edits = Vec::new();
        for (from, to) in moves {
            if from == to || !seen.insert(from.as_ref()) {
                continue;
            }
            for (document, kind) in self.document_dependencies().used_by(from) {
                match kind {
                    DependencyKind::Import | DependencyKind::Dictionaries => {
                        edits.push(PathEdit::ReplaceImport {
                            document,
                            from: from.clone(),
                            to: to.clone(),
                        });
                    }
                    DependencyKind::Signature => {
                        edits.extend(self.decision_key_edits(&document, from, to));
                    }
                }
            }
        }
        edits
    }

    fn decision_key_edits(
        &self,
        document: &Arc<str>,
        from: &Arc<str>,
        to: &Arc<str>,
    ) -> Vec<PathEdit> {
        let Some(content) = self.raw_document(document) else {
            return Vec::new();
        };
        let DecisionContent::Graph(graph) = content.as_ref() else {
            return Vec::new();
        };
        graph
            .nodes
            .iter()
            .filter(|node| {
                matches!(&node.kind, DecisionNodeKind::DecisionNode { content } if content.key == *from)
            })
            .map(|node| PathEdit::ReplaceDecisionKey {
                document: document.clone(),
                node_id: node.id.clone(),
                from: from.clone(),
                to: to.clone(),
            })
            .collect()
    }
}

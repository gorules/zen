use std::sync::Arc;

use crate::workspace::db::Db;
use crate::workspace::graph::analysis::GraphAnalyzer;
use crate::workspace::types::{Cursor, ExpressionKind, NlExpression};

impl Db {
    pub(crate) fn graph_nl(&self, path: &str) -> Vec<NlExpression> {
        let path_arc: Arc<str> = Arc::from(path);
        let snap = self.snapshot();
        let Some(doc) = snap.graphs.get(&path_arc).cloned() else {
            return Vec::new();
        };
        let Some(content) = doc.as_graph() else {
            return Vec::new();
        };
        let Some(analysis) = self.graph_analysis(&path_arc) else {
            return Vec::new();
        };

        let intellisense = self.graph_intellisense();
        intellisense
            .borrow_mut()
            .set_nl_labels(self.nl_label_resolver(path));
        let mut out = Vec::new();
        for node in &content.nodes {
            let Some(node_analysis) = analysis.nodes.get(&node.id) else {
                continue;
            };
            for site in GraphAnalyzer::node_sites(node) {
                let cursor = Cursor {
                    policy_path: path_arc.clone(),
                    block_id: node.id.clone(),
                    pos: 0,
                    target: site.target.clone(),
                };
                let Some((source, kind, scope)) =
                    self.resolve_in_node(node, node_analysis, &cursor)
                else {
                    continue;
                };
                let expected = (!matches!(kind, ExpressionKind::Unary))
                    .then(|| self.graph_cell_expected(&cursor))
                    .flatten();
                out.push(NlExpression::project_expected(
                    &mut intellisense.borrow_mut(),
                    &path_arc,
                    &node.id,
                    site.target,
                    kind,
                    &source,
                    &scope,
                    expected.as_ref(),
                ));
            }
        }
        intellisense.borrow_mut().set_nl_labels(None);
        out
    }
}

use std::sync::Arc;

use crate::workspace::db::Db;
use crate::workspace::document_dependencies::DependencyKind;

impl Db {
    pub fn affected_by(&self, paths: &[&str]) -> Vec<Arc<str>> {
        let changed: Vec<Arc<str>> = paths.iter().map(|path| Arc::from(*path)).collect();
        self.document_dependencies()
            .affected(&changed, |user, dependency, kind| match kind {
                DependencyKind::Import => true,
                DependencyKind::Dictionaries | DependencyKind::Signature => {
                    match self.recorded_view(user, dependency, kind) {
                        Some(view) => !self.view_holds(dependency, &view),
                        None => true,
                    }
                }
            })
    }
}

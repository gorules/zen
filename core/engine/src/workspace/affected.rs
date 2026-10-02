use std::sync::Arc;

use crate::workspace::db::Db;
use crate::workspace::document_dependencies::DependencyKind;
use crate::workspace::reads::ReadView;

impl Db {
    pub(crate) fn walk(&self, changed: &[Arc<str>]) -> Vec<Arc<str>> {
        let frozen = self.frozen_views();
        let cycles = self.document_dependencies().signature_cycles();
        self.document_dependencies()
            .affected(changed, |user, dependency, kind| {
                let recorded = frozen
                    .get(&(user.clone(), dependency.clone()))
                    .into_iter()
                    .flatten()
                    .find(|view| {
                        matches!(
                            (kind, view),
                            (DependencyKind::Dictionaries, ReadView::Dictionaries(_))
                                | (DependencyKind::Signature, ReadView::Signature(_))
                        )
                    });
                let cyclic = kind == DependencyKind::Signature
                    && cycles
                        .get(user)
                        .is_some_and(|component| cycles.get(dependency) == Some(component));
                match (kind, recorded) {
                    (DependencyKind::Import, _) | (_, None) => true,
                    _ if cyclic => true,
                    (_, Some(view)) => {
                        self.graph_stack.borrow_mut().push(user.clone());
                        let holds = self.view_holds(dependency, view);
                        self.graph_stack.borrow_mut().pop();
                        !holds
                    }
                }
            })
    }
}

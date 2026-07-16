use std::sync::Arc;

use ahash::HashSet;

#[derive(Debug, Clone)]
pub enum PathRoot {
    Entity { entity: Arc<str> },
    Global { name: Arc<str> },
}

#[derive(Debug, Clone)]
pub struct PathClassifier {
    entities: HashSet<Arc<str>>,
}

impl PathClassifier {
    pub fn new(entities: HashSet<Arc<str>>) -> Self {
        Self { entities }
    }

    pub fn classify(&self, path: &str) -> PathRoot {
        let first = path.split_once('.').map_or(path, |(f, _)| f);
        if self.entities.contains(first) {
            return PathRoot::Entity {
                entity: Arc::from(first),
            };
        }
        PathRoot::Global {
            name: Arc::from(first),
        }
    }
}

impl PathClassifier {
    pub(crate) fn from_data_models<'a>(
        models: impl IntoIterator<Item = &'a crate::policy::ir::DataModelIr>,
    ) -> Self {
        let entities = models
            .into_iter()
            .filter(|dm| !dm.scope.is_global())
            .map(|dm| dm.name.clone())
            .collect();
        Self::new(entities)
    }
}

impl crate::workspace::db::Snapshot {
    pub(crate) fn compute_path_classifier(
        all_parsed: &ahash::HashMap<Arc<str>, Arc<crate::policy::ir::ParsedPolicy>>,
    ) -> PathClassifier {
        PathClassifier::from_data_models(
            all_parsed
                .values()
                .flat_map(|p| p.policy.data_models())
                .map(|(_, dm)| dm),
        )
    }
}

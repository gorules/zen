use crate::decision::Decision;
use crate::loader::DynamicLoader;
use crate::model::GraphContent;
use crate::policy::evaluator::EvalArtifact;
use crate::policy::raw::PolicyDocument;
use ahash::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
pub(crate) struct Prepared {
    graphs: Mutex<HashMap<Arc<str>, Kept>>,
}

#[derive(Debug)]
struct Kept {
    source: Arc<GraphContent>,
    ready: Arc<GraphContent>,
}

impl Prepared {
    pub async fn graph(
        &self,
        key: &str,
        source: Arc<GraphContent>,
        loader: &DynamicLoader,
    ) -> Arc<GraphContent> {
        if let Some(ready) = self.kept(key, &source) {
            return ready;
        }
        let ready = GraphContent::loaded(source.clone(), loader).await;
        if !ready.schemas_failed() {
            if let Ok(mut graphs) = self.graphs.lock() {
                graphs.insert(
                    Arc::from(key),
                    Kept {
                        source,
                        ready: ready.clone(),
                    },
                );
            }
        }
        ready
    }

    fn kept(&self, key: &str, source: &Arc<GraphContent>) -> Option<Arc<GraphContent>> {
        let graphs = self.graphs.lock().ok()?;
        let kept = graphs.get(key)?;
        Arc::ptr_eq(&kept.source, source).then(|| kept.ready.clone())
    }
}

#[derive(Debug, Default)]
pub(crate) struct Decisions {
    decisions: Mutex<HashMap<Arc<str>, Arc<Decision>>>,
}

impl Decisions {
    pub fn get(
        &self,
        key: &str,
        source: &Arc<GraphContent>,
        build: impl FnOnce() -> Decision,
    ) -> Arc<Decision> {
        if let Ok(decisions) = self.decisions.lock() {
            if let Some(decision) = decisions.get(key) {
                if decision.holds(source) {
                    return decision.clone();
                }
            }
        }
        let decision = Arc::new(build());
        if let Ok(mut decisions) = self.decisions.lock() {
            decisions.insert(Arc::from(key), decision.clone());
        }
        decision
    }
}

type Documents = Vec<(Arc<str>, Arc<PolicyDocument>)>;

#[derive(Default)]
pub(crate) struct Policies {
    artifacts: Mutex<HashMap<Arc<str>, KeptPolicy>>,
}

struct KeptPolicy {
    documents: Documents,
    artifact: Option<Arc<EvalArtifact>>,
}

impl std::fmt::Debug for Policies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policies").finish_non_exhaustive()
    }
}

impl Policies {
    pub fn artifact(
        &self,
        key: &str,
        documents: Documents,
        build: impl FnOnce(&Documents) -> Option<Arc<EvalArtifact>>,
    ) -> Option<Arc<EvalArtifact>> {
        if let Ok(artifacts) = self.artifacts.lock() {
            if let Some(kept) = artifacts.get(key) {
                if Self::same(&kept.documents, &documents) {
                    return kept.artifact.clone();
                }
            }
        }
        let artifact = build(&documents);
        if let Ok(mut artifacts) = self.artifacts.lock() {
            artifacts.insert(
                Arc::from(key),
                KeptPolicy {
                    documents,
                    artifact: artifact.clone(),
                },
            );
        }
        artifact
    }

    fn same(kept: &Documents, loaded: &Documents) -> bool {
        kept.len() == loaded.len()
            && kept
                .iter()
                .zip(loaded)
                .all(|((a, x), (b, y))| a == b && Arc::ptr_eq(x, y))
    }
}

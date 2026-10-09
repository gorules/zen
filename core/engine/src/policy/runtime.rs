use std::collections::VecDeque;
use std::sync::Arc;

use ahash::{HashMap, HashSet, HashSetExt};
use zen_expression::variable::Variable;

use crate::decision::Decision;
use crate::decision_graph::graph::{DecisionGraphResponse, EvaluationTrace};
use crate::engine::EvaluationOptions;
use crate::loader::DynamicLoader;
use crate::model::{DecisionContent, GraphContent};
use crate::policy::evaluator::EvalArtifact;
use crate::policy::raw::PolicyDocument;
use crate::workspace::types::{
    Diagnostic, EvaluateRequest, EvaluationError as PolicyEvaluationError, Severity,
};
use crate::workspace::Workspace;
use crate::{CompileFailure, EvaluationError};

pub(crate) async fn evaluate_policy(
    loader: &DynamicLoader,
    entry_key: &str,
    entry_content: Arc<DecisionContent>,
    input: Variable,
    options: EvaluationOptions,
) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
    let entry_path: Arc<str> = Arc::from(entry_key);

    let documents = collect_transitive_policies(loader, entry_path.clone(), entry_content).await?;

    let mut workspace = Workspace::new();
    for (path, doc) in documents {
        workspace.set_policy_arc(path, doc);
    }

    let blocking_errors = workspace
        .evaluation_diagnostics(&entry_path)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    if blocking_errors > 0 {
        return Err(Box::new(EvaluationError::Policy(
            PolicyEvaluationError::CompilationErrors {
                policy_path: entry_path.clone(),
            },
        )));
    }

    let request = EvaluateRequest {
        policy_path: entry_path,
        input,
        goals: vec![],
        trace: options.trace,
    };

    let result = workspace
        .evaluate(&request)
        .map_err(|e| Box::new(EvaluationError::Policy(e)))?;

    Ok(DecisionGraphResponse {
        performance: format!("{:.1?}", result.duration),
        result: result.output,
        trace: result.trace.map(EvaluationTrace::Policy),
    })
}

async fn collect_transitive_policies(
    loader: &DynamicLoader,
    entry_path: Arc<str>,
    entry_content: Arc<DecisionContent>,
) -> Result<Vec<(Arc<str>, Arc<PolicyDocument>)>, Box<EvaluationError>> {
    let mut documents: Vec<(Arc<str>, Arc<PolicyDocument>)> = Vec::new();
    let mut enqueued: HashSet<Arc<str>> = HashSet::new();
    let mut queue: VecDeque<(Arc<str>, Arc<DecisionContent>)> = VecDeque::new();

    enqueued.insert(entry_path.clone());
    queue.push_back((entry_path, entry_content));

    while let Some((path, content)) = queue.pop_front() {
        let doc = match content.as_ref() {
            DecisionContent::Policy(policy) => policy.0.clone(),
            DecisionContent::Graph(_) => {
                return Err(Box::new(EvaluationError::ContentKindMismatch {
                    expected: "policy",
                    got: "graph",
                    key: path,
                }));
            }
        };

        for import_path in &doc.imports {
            if !enqueued.insert(import_path.clone()) {
                continue;
            }
            let next = loader
                .load(import_path.as_ref())
                .await
                .map_err(|e| Box::<EvaluationError>::from(e))?;
            queue.push_back((import_path.clone(), next));
        }

        documents.push((path, doc));
    }

    Ok(documents)
}

#[derive(Clone)]
pub(crate) enum CompiledEntry {
    Policy(Arc<EvalArtifact>),
    Graph(Arc<GraphContent>),
}

pub(crate) struct CompiledSet {
    entries: HashMap<Arc<str>, CompiledEntry>,
    failures: Vec<CompileFailure>,
    /// What each compiled document reads from its request (None: unknown).
    reads: HashMap<Arc<str>, Option<std::collections::BTreeSet<String>>>,
}

impl CompiledSet {
    pub(crate) fn build_sync(loader: &DynamicLoader, keys: &[Arc<str>]) -> CompiledSet {
        let mut workspace = Workspace::new();
        let mut policy_keys: Vec<Arc<str>> = Vec::new();
        let mut failures: Vec<CompileFailure> = Vec::new();
        let mut entries: HashMap<Arc<str>, CompiledEntry> = HashMap::default();

        for key in keys {
            let Some(load_result) = loader.load_sync(key.as_ref()) else {
                continue;
            };
            let content = match load_result {
                Ok(content) => content,
                Err(error) => {
                    failures.push(CompileFailure {
                        key: key.clone(),
                        kind: "load",
                        diagnostics: Vec::new(),
                        error: Some(error.to_string()),
                    });
                    continue;
                }
            };
            match content.as_ref() {
                DecisionContent::Policy(policy) => {
                    workspace.set_policy_arc(key.clone(), policy.0.clone());
                    policy_keys.push(key.clone());
                }
                DecisionContent::Graph(graph) => match Decision::from(graph.clone()).validate() {
                    Err(error) => failures.push(CompileFailure {
                        key: key.clone(),
                        kind: "graph",
                        diagnostics: Vec::new(),
                        error: Some(error.to_string()),
                    }),
                    Ok(()) => {
                        workspace.set_document_arc(key.clone(), content.clone());
                        let mut compiled = graph.clone();
                        Arc::make_mut(&mut compiled).compile();
                        entries.insert(key.clone(), CompiledEntry::Graph(compiled));
                    }
                },
            }
        }

        let errors: HashMap<Arc<str>, Vec<Diagnostic>> = policy_keys
            .iter()
            .map(|key| (key.clone(), Self::error_diagnostics(&workspace, key)))
            .collect();

        for key in &policy_keys {
            let diagnostics = &errors[key];
            if diagnostics.is_empty() {
                entries.insert(
                    key.clone(),
                    CompiledEntry::Policy(workspace.eval_artifact(key)),
                );
                continue;
            }
            let valid_through_importer = policy_keys.iter().any(|other| {
                other != key && errors[other].is_empty() && workspace.imports(other, key)
            });
            failures.push(CompileFailure {
                key: key.clone(),
                kind: if valid_through_importer {
                    "policyImportOnly"
                } else {
                    "policy"
                },
                diagnostics: diagnostics.clone(),
                error: None,
            });
        }

        let reads = entries.keys().map(|key| (key.clone(), workspace.reads(key))).collect();
        CompiledSet { entries, failures, reads }
    }

    pub(crate) fn reads(&self, key: &str) -> Option<std::collections::BTreeSet<String>> {
        let key = self.key_of(key)?;
        self.reads.get(&key).cloned().flatten()
    }

    /// The key a document is compiled under: as given, or with/without
    /// `.json` (loaders find documents either way).
    pub(crate) fn key_of(&self, key: &str) -> Option<Arc<str>> {
        let candidates = [
            Some(key.to_string()),
            key.strip_suffix(".json").map(str::to_string),
            (!key.ends_with(".json")).then(|| format!("{key}.json")),
        ];
        candidates.into_iter().flatten().find_map(|candidate| {
            self.entries
                .get_key_value(candidate.as_str())
                .map(|(k, _)| k.clone())
        })
    }

    fn error_diagnostics(workspace: &Workspace, key: &Arc<str>) -> Vec<Diagnostic> {
        workspace
            .evaluation_diagnostics(key)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .collect()
    }

    pub(crate) fn get(&self, key: &str) -> Option<CompiledEntry> {
        self.entries.get(key).cloned()
    }

    pub(crate) fn failures(&self) -> &[CompileFailure] {
        &self.failures
    }
}

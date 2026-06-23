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
use crate::policy::types::{
    Diagnostic, EvaluateRequest, EvaluationError as PolicyEvaluationError, Severity,
};
use crate::policy::workspace::PolicyWorkspace;
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

    let mut workspace = PolicyWorkspace::new();
    for (path, doc) in documents {
        workspace.set_policy_arc(path, doc);
    }

    let blocking_errors = workspace
        .all_diagnostics()
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
}

impl CompiledSet {
    pub(crate) fn build_sync(loader: &DynamicLoader, keys: &[Arc<str>]) -> CompiledSet {
        let mut workspace = PolicyWorkspace::new();
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
                DecisionContent::Graph(graph) => {
                    match Decision::from(Arc::new(graph.clone())).validate() {
                        Err(error) => failures.push(CompileFailure {
                            key: key.clone(),
                            kind: "graph",
                            diagnostics: Vec::new(),
                            error: Some(error.to_string()),
                        }),
                        Ok(()) => {
                            let mut compiled = graph.clone();
                            compiled.compile();
                            entries.insert(key.clone(), CompiledEntry::Graph(Arc::new(compiled)));
                        }
                    }
                }
            }
        }

        for key in &policy_keys {
            let diagnostics = Self::closure_error_diagnostics(&workspace, key);
            if diagnostics.is_empty() {
                entries.insert(
                    key.clone(),
                    CompiledEntry::Policy(workspace.eval_artifact(key)),
                );
            } else {
                failures.push(CompileFailure {
                    key: key.clone(),
                    kind: "policy",
                    diagnostics,
                    error: None,
                });
            }
        }

        CompiledSet { entries, failures }
    }

    fn closure_error_diagnostics(workspace: &PolicyWorkspace, key: &Arc<str>) -> Vec<Diagnostic> {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let mut enqueued: HashSet<Arc<str>> = HashSet::new();
        let mut queue: VecDeque<Arc<str>> = VecDeque::new();

        enqueued.insert(key.clone());
        queue.push_back(key.clone());

        while let Some(path) = queue.pop_front() {
            diagnostics.extend(
                workspace
                    .diagnostics(path.as_ref())
                    .into_iter()
                    .filter(|d| d.severity == Severity::Error),
            );
            if let Some(doc) = workspace.get_policy(path.as_ref()) {
                for import_path in &doc.imports {
                    if enqueued.insert(import_path.clone()) {
                        queue.push_back(import_path.clone());
                    }
                }
            }
        }

        diagnostics
    }

    pub(crate) fn get(&self, key: &str) -> Option<CompiledEntry> {
        self.entries.get(key).cloned()
    }

    pub(crate) fn failures(&self) -> &[CompileFailure] {
        &self.failures
    }
}

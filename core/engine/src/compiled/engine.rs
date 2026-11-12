use crate::compiled::decision::CompiledDecision;
use crate::compiled::host::graph::graph::Responses;
use crate::compiled::pinning;
use crate::compiled::policy::artifact::Artifact;
use crate::compiled::policy::validator::InputSchema;
use crate::compiled::prepared::{Decisions, Policies, Prepared};
use crate::decision_graph::graph::DecisionGraphResponse;
use crate::engine::{EvaluationOptions, EvaluationSerializedOptions};
use crate::loader::{DynamicLoader, LoaderResult};
use crate::model::{DecisionContent, GraphContent};
use crate::nodes::custom::DynamicCustomNode;
use crate::nodes::function::http_handler::DynamicHttpHandler;
use crate::policy::raw::PolicyDocument;
use crate::workspace::db::Db;
use crate::workspace::types::{
    EvaluateRequest, EvaluationError as PolicyEvaluationError, Severity,
};
use crate::EvaluationError;
use ahash::{HashMap, HashSet, HashSetExt};
use arc_swap::ArcSwapOption;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
use zen_expression::context::EvaluationContext;
use zen_expression::variable::Variable;

pub(crate) struct Parts<'e> {
    pub(crate) loader: &'e DynamicLoader,
    pub(crate) adapter: &'e DynamicCustomNode,
    pub(crate) http_handler: &'e DynamicHttpHandler,
}

#[derive(Clone)]
enum Entry {
    Policy(Arc<Artifact>),
    Graph(Arc<GraphContent>),
}

#[derive(Default)]
struct Compiled {
    entries: HashMap<Arc<str>, Entry>,
}

#[derive(Default)]
pub(crate) struct Caches {
    children: Arc<Prepared>,
    decisions: Decisions,
    policies: Policies,
    compiled: ArcSwapOption<Compiled>,
}

impl std::fmt::Debug for Caches {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caches").finish_non_exhaustive()
    }
}

impl Caches {
    pub(crate) fn compile(&self, loader: &DynamicLoader, keys: &[Arc<str>]) {
        let mut db = Db::new();
        let mut policies: Vec<Arc<str>> = Vec::new();
        let mut entries: HashMap<Arc<str>, Entry> = HashMap::default();
        for key in keys {
            let Some(Ok(content)) = loader.load_sync(key.as_ref()) else {
                continue;
            };
            match content.as_ref() {
                DecisionContent::Policy(policy) => {
                    db.set_policy(key.clone(), policy.0.clone());
                    policies.push(key.clone());
                }
                DecisionContent::Graph(graph) => {
                    entries.insert(key.clone(), Entry::Graph(graph.clone()));
                }
            }
        }
        for key in &policies {
            let blocked = db
                .evaluation_diagnostics(key)
                .into_iter()
                .any(|d| d.severity == Severity::Error);
            if !blocked {
                let artifact = Artifact::new(db.eval_artifact(key), InputSchema::of(&db, key));
                entries.insert(key.clone(), Entry::Policy(Arc::new(artifact)));
            }
        }
        self.compiled.store(Some(Arc::new(Compiled { entries })));
    }

    fn entry(&self, key: &str) -> Option<Entry> {
        self.compiled
            .load_full()
            .and_then(|compiled| compiled.entries.get(key).cloned())
    }

    fn decision(
        &self,
        parts: &Parts,
        key: &str,
        graph: &Arc<GraphContent>,
    ) -> Arc<CompiledDecision> {
        self.decisions.get(key, graph, || {
            CompiledDecision::new(
                graph.clone(),
                parts.loader.clone(),
                parts.adapter.clone(),
                parts.http_handler.clone(),
                self.children.clone(),
            )
        })
    }

    pub(crate) async fn evaluate_pinned(
        &self,
        parts: &Parts<'_>,
        key: &str,
        input: Variable,
        options: EvaluationOptions,
    ) -> LoaderResult<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let context = pinning::single(options.pinned());
        self.evaluate(parts, key, input, options, &context).await
    }

    pub(crate) async fn evaluate_serialized_pinned(
        &self,
        parts: &Parts<'_>,
        key: &str,
        input: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value> {
        let context = pinning::single(options.pinned());
        self.evaluate_serialized(parts, key, input, options, &context)
            .await
    }

    pub(crate) async fn evaluate_batch_pinned(
        &self,
        parts: &Parts<'_>,
        key: &str,
        contexts: &[Variable],
        options: EvaluationOptions,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        let context = pinning::batch(options.pinned());
        self.evaluate_batch(parts, key, contexts, options, &context)
            .await
    }

    pub(crate) async fn evaluate(
        &self,
        parts: &Parts<'_>,
        key: &str,
        input: Variable,
        options: EvaluationOptions,
        context: &EvaluationContext,
    ) -> LoaderResult<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        if let Some(entry) = self.entry(key) {
            return Ok(match entry {
                Entry::Policy(artifact) => artifact
                    .evaluate_entry(key, input, options.trace, context)
                    .map(Responses::policy)
                    .map_err(|e| Box::new(EvaluationError::Policy(e))),
                Entry::Graph(graph) => {
                    self.decision(parts, key, &graph)
                        .evaluate_with_opts(input, options, context)
                        .await
                }
            });
        }
        let content = parts.loader.load(key).await?;
        Ok(match content.as_ref() {
            DecisionContent::Graph(graph) => {
                self.decision(parts, key, graph)
                    .evaluate_with_opts(input, options, context)
                    .await
            }
            DecisionContent::Policy(_) => {
                self.policy(parts.loader, key, content.clone(), input, options, context)
                    .await
            }
        })
    }

    pub(crate) async fn evaluate_serialized(
        &self,
        parts: &Parts<'_>,
        key: &str,
        input: Variable,
        options: EvaluationSerializedOptions,
        context: &EvaluationContext,
    ) -> Result<Value, Value> {
        match self
            .evaluate(parts, key, input, options.unserialized(), context)
            .await
        {
            Ok(response) => Responses::serialized(response, options.trace),
            Err(err) => Err(Value::String(err.to_string())),
        }
    }

    pub(crate) async fn evaluate_batch(
        &self,
        parts: &Parts<'_>,
        key: &str,
        contexts: &[Variable],
        options: EvaluationOptions,
        context: &EvaluationContext,
    ) -> Vec<Result<DecisionGraphResponse, Box<EvaluationError>>> {
        if let Some(entry) = self.entry(key) {
            return match entry {
                Entry::Policy(artifact) => {
                    let requests: Vec<EvaluateRequest> = contexts
                        .iter()
                        .map(|input| EvaluateRequest {
                            policy_path: Arc::from(key),
                            input: input.clone(),
                            goals: Vec::new(),
                            trace: options.trace,
                        })
                        .collect();
                    artifact
                        .evaluate_batch(&requests, context)
                        .into_iter()
                        .map(|result| {
                            result
                                .map(Responses::policy)
                                .map_err(|e| Box::new(EvaluationError::Policy(e)))
                        })
                        .collect()
                }
                Entry::Graph(graph) => {
                    self.decision(parts, key, &graph)
                        .evaluate_batch(contexts, options, context)
                        .await
                }
            };
        }
        if let Ok(content) = parts.loader.load(key).await {
            if let DecisionContent::Graph(graph) = content.as_ref() {
                return self
                    .decision(parts, key, graph)
                    .evaluate_batch(contexts, options, context)
                    .await;
            }
        }
        let mut results = Vec::with_capacity(contexts.len());
        for (row, input) in contexts.iter().enumerate() {
            results.push(
                match Box::pin(self.evaluate(
                    parts,
                    key,
                    input.deep_clone(),
                    options,
                    &context.at(row),
                ))
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(Box::<EvaluationError>::from(error)),
                },
            );
        }
        results
    }

    async fn policy(
        &self,
        loader: &DynamicLoader,
        entry_key: &str,
        entry_content: Arc<DecisionContent>,
        input: Variable,
        options: EvaluationOptions,
        context: &EvaluationContext,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let entry_path: Arc<str> = Arc::from(entry_key);
        let documents = Self::documents(loader, entry_path.clone(), entry_content).await?;

        let artifact = self.policies.artifact(&entry_path, documents, |documents| {
            let mut db = Db::new();
            for (path, doc) in documents {
                db.set_policy(path.clone(), doc.clone());
            }
            let blocked = db
                .evaluation_diagnostics(&entry_path)
                .into_iter()
                .any(|d| d.severity == Severity::Error);
            (!blocked).then(|| {
                Arc::new(Artifact::new(
                    db.eval_artifact(&entry_path),
                    InputSchema::of(&db, &entry_path),
                ))
            })
        });
        let Some(artifact) = artifact else {
            return Err(Box::new(EvaluationError::Policy(
                PolicyEvaluationError::CompilationErrors {
                    policy_path: entry_path.clone(),
                },
            )));
        };

        let result = artifact
            .evaluate_entry(&entry_path, input, options.trace, context)
            .map_err(|e| Box::new(EvaluationError::Policy(e)))?;

        Ok(Responses::policy(result))
    }

    async fn documents(
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
                    .map_err(Box::<EvaluationError>::from)?;
                queue.push_back((import_path.clone(), next));
            }

            documents.push((path, doc));
        }

        Ok(documents)
    }
}

use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::engine::{EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind};
use crate::loader::{DynamicLoader, NoopLoader};
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::validator_cache::ValidatorCache;
use crate::nodes::NodeHandlerExtensions;
use crate::{DecisionGraphValidationError, EvaluationError};
use ahash::{HashMap, HashMapExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::OnceCell;
use std::sync::Arc;
use zen_expression::compiler::Opcode;
use zen_expression::variable::Variable;
use zen_expression::{ExpressionKind, Isolate};
use zen_types::decision::{DecisionEdge, DecisionNode, DecisionNodeKind};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CompilationKey {
    pub kind: ExpressionKind,
    pub source: Arc<str>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DecisionContent {
    pub nodes: Vec<Arc<DecisionNode>>,
    pub edges: Vec<Arc<DecisionEdge>>,

    #[serde(skip)]
    pub compiled_cache: Option<Arc<HashMap<CompilationKey, Vec<Opcode>>>>,
}

impl DecisionContent {
    pub fn compile(&mut self) {
        let mut compiled_cache: HashMap<CompilationKey, Vec<Opcode>> = HashMap::new();
        let mut isolate = Isolate::new();

        for node in &self.nodes {
            match &node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    for expression in content.expressions.iter() {
                        if expression.key.is_empty() || expression.value.is_empty() {
                            continue;
                        }

                        let key = CompilationKey {
                            kind: ExpressionKind::Standard,
                            source: Arc::clone(&expression.value),
                        };

                        if compiled_cache.contains_key(&key) {
                            continue;
                        }

                        if let Ok(comp_expression) = isolate.compile_standard(&expression.value) {
                            compiled_cache.insert(key, comp_expression.bytecode().to_vec());
                        }
                    }
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    for rule in content.rules.iter() {
                        for input in content.inputs.iter() {
                            let Some(rule_value) = rule.get(&input.id) else {
                                continue;
                            };

                            if rule_value.is_empty() {
                                continue;
                            }

                            match &input.field {
                                None => {
                                    let key = CompilationKey {
                                        kind: ExpressionKind::Standard,
                                        source: Arc::clone(rule_value),
                                    };

                                    if !compiled_cache.contains_key(&key) {
                                        if let Ok(comp_expression) =
                                            isolate.compile_standard(rule_value)
                                        {
                                            compiled_cache
                                                .insert(key, comp_expression.bytecode().to_vec());
                                        }
                                    }
                                }
                                Some(_field) => {
                                    let key = CompilationKey {
                                        kind: ExpressionKind::Unary,
                                        source: Arc::clone(rule_value),
                                    };

                                    if !compiled_cache.contains_key(&key) {
                                        if let Ok(comp_expression) =
                                            isolate.compile_unary(rule_value)
                                        {
                                            compiled_cache
                                                .insert(key, comp_expression.bytecode().to_vec());
                                        }
                                    }
                                }
                            }
                        }

                        for output in content.outputs.iter() {
                            let Some(rule_value) = rule.get(&output.id) else {
                                continue;
                            };

                            if rule_value.is_empty() {
                                continue;
                            }

                            let key = CompilationKey {
                                kind: ExpressionKind::Standard,
                                source: Arc::clone(rule_value),
                            };

                            if !compiled_cache.contains_key(&key) {
                                if let Ok(comp_expression) = isolate.compile_standard(rule_value) {
                                    compiled_cache.insert(key, comp_expression.bytecode().to_vec());
                                }
                            }
                        }
                    }
                }

                _ => {}
            }
        }

        self.compiled_cache.replace(Arc::new(compiled_cache));
    }
}

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<DecisionContent>,
    loader: DynamicLoader,
    adapter: DynamicCustomNode,
    validator_cache: ValidatorCache,
}

impl From<DecisionContent> for Decision {
    fn from(value: DecisionContent) -> Self {
        Self {
            content: value.into(),
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            validator_cache: ValidatorCache::default(),
        }
    }
}

impl From<Arc<DecisionContent>> for Decision {
    fn from(value: Arc<DecisionContent>) -> Self {
        Self {
            content: value,
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            validator_cache: ValidatorCache::default(),
        }
    }
}

impl Decision {
    pub fn with_loader(mut self, loader: DynamicLoader) -> Self {
        self.loader = loader;
        self
    }

    pub fn with_adapter(mut self, adapter: DynamicCustomNode) -> Self {
        self.adapter = adapter;
        self
    }

    /// Evaluates a decision using an in-memory reference stored in struct
    pub async fn evaluate(
        &self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        self.evaluate_with_opts(context, Default::default()).await
    }

    /// Evaluates a decision using in-memory reference with advanced options
    pub async fn evaluate_with_opts(
        &self,
        context: Variable,
        options: EvaluationOptions,
    ) -> Result<DecisionGraphResponse, Box<EvaluationError>> {
        let mut decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: options.max_depth,
            trace: options.trace,
            iteration: 0,
            extensions: NodeHandlerExtensions {
                loader: self.loader.clone(),
                custom_node: self.adapter.clone(),
                compiled_cache: self.content.compiled_cache.clone(),
                validator_cache: Arc::new(OnceCell::from(self.validator_cache.clone())),
                ..Default::default()
            },
        })?;
        let response = decision_graph.evaluate(context).await?;

        Ok(response)
    }

    pub async fn evaluate_serialized(
        &self,
        context: Variable,
        options: EvaluationSerializedOptions,
    ) -> Result<Value, Value> {
        let response = self
            .evaluate_with_opts(
                context,
                EvaluationOptions {
                    trace: options.trace != EvaluationTraceKind::None,
                    max_depth: options.max_depth,
                },
            )
            .await;

        match response {
            Ok(ok) => Ok(ok
                .serialize_with_mode(serde_json::value::Serializer, options.trace)
                .unwrap_or_default()),
            Err(err) => Err(err
                .serialize_with_mode(serde_json::value::Serializer, options.trace)
                .unwrap_or_default()),
        }
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let decision_graph = DecisionGraph::try_new(DecisionGraphConfig {
            content: self.content.clone(),
            max_depth: 1,
            trace: false,
            iteration: 0,
            extensions: Default::default(),
        })?;

        decision_graph.validate()
    }

    pub fn compile(&mut self) -> () {
        let cm = Arc::make_mut(&mut self.content);
        cm.compile();
    }
}

use crate::decision_graph::graph::{DecisionGraph, DecisionGraphConfig, DecisionGraphResponse};
use crate::engine::{EvaluationOptions, EvaluationSerializedOptions, EvaluationTraceKind};
use crate::loader::{DynamicLoader, NoopLoader};
use crate::model::DecisionContent;
use crate::nodes::custom::{DynamicCustomNode, NoopCustomNode};
use crate::nodes::validator_cache::ValidatorCache;
use crate::nodes::NodeHandlerExtensions;
use crate::{DecisionGraphValidationError, EvaluationError};
use serde_json::Value;
use std::cell::{OnceCell, RefCell};
use std::sync::{Arc, Mutex};
use ahash::{HashMap, HashMapExt};
use zen_expression::compiler::Opcode;
use zen_expression::{ExpressionKind, Isolate};
use zen_expression::variable::Variable;
use zen_types::decision::{DecisionNode, DecisionNodeKind, ExpressionNodeContent};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CompilationKey {
    pub kind: ExpressionKind,
    pub source: Arc<str>
}

/// Represents a JDM decision which can be evaluated
#[derive(Debug, Clone)]
pub struct Decision {
    content: Arc<DecisionContent>,
    loader: DynamicLoader,
    adapter: DynamicCustomNode,
    validator_cache: ValidatorCache,
    compiled_cache: Arc<RefCell<HashMap<CompilationKey, Vec<Opcode>>>>,
}

impl From<DecisionContent> for Decision {
    fn from(value: DecisionContent) -> Self {
        Self {
            content: value.into(),
            loader: Arc::new(NoopLoader::default()),
            adapter: Arc::new(NoopCustomNode::default()),
            validator_cache: ValidatorCache::default(),
            compiled_cache: Arc::new(RefCell::new(HashMap::new())),
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
            compiled_cache: Arc::new(RefCell::new(HashMap::new())),
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



    //noinspection DuplicatedCode
    // i onda imas compile_decision npr koji prodje kroz DecisionContent i za
    // svaki expression koji postoji uradi .compile_standard ili compile_unary pa storuje u compiled_cache
    pub fn compile_decision(
        &self,
    ) -> () {
        let output_nodes: Vec<(Arc<DecisionNode>, &ExpressionNodeContent)> = self.content.nodes
            .iter()
            .filter_map(|node| {
                if let DecisionNodeKind::ExpressionNode { ref content } = node.kind {
                    Some((Arc::clone(node), content))
                } else {
                    None
                }
            })
            .collect();
        let mut isolate = Isolate::new();
        for (_node, content) in &output_nodes {
            for expression in content.expressions.iter() {
                if expression.key.is_empty() || expression.value.is_empty() {
                    continue;
                }

                if let Ok(comp_expression) = isolate.compile_standard(&expression.value) {
                    let key = CompilationKey {
                        kind: ExpressionKind::Standard,
                        source: Arc::from(expression.value.clone()),
                    };
                    self.compiled_cache.borrow_mut()
                        .entry(key.clone())
                        .or_insert(comp_expression.bytecode().to_vec());
                }

            }

        }


    }



    //noinspection DuplicatedCode
    // pub fn compile_decision_debug(
    //     &self,
    // ) -> () {
    //     let output_nodes: Vec<(Arc<DecisionNode>, &ExpressionNodeContent)> = self.content.nodes
    //         .iter()
    //         .filter_map(|node| {
    //             if let DecisionNodeKind::ExpressionNode { ref content } = node.kind {
    //                 Some((Arc::clone(node), content))
    //             } else {
    //                 None
    //             }
    //         })
    //         .collect();
    //     let times = 10_000;
    //     let mut isolate = Isolate::new();
    //     for (_node, content) in &output_nodes {
    //         for expression in content.expressions.iter() {
    //             println!(" ");
    //             println!("Expr ******************************** {:?}", expression);
    //             if expression.key.is_empty() || expression.value.is_empty() {
    //                 continue;
    //             }
    //
    //             benchmark("Compile Standard", times, || {
    //                 let comp = isolate.compile_standard(&expression.value);
    //             });
    //
    //
    //             let comp = isolate.compile_standard(&expression.value);
    //             if let Ok(compExpression) = comp {
    //                 let key = CompilationKey {
    //                     kind: ExpressionKind::Standard,
    //                     source: Arc::from(expression.value.clone()),
    //                 };
    //                 // println!("Compiled Expression {:?}", compExpression);
    //
    //                 // using rmp-Serde
    //                 // let bytes = rmp_serde::to_vec(compExpression.bytecode().as_ref()).unwrap();
    //                 // self.content.compiled_cache.lock().unwrap()
    //                 //     .entry(key.clone())
    //                 //     .or_insert(bytes);
    //                 //
    //                 // benchmark("Deserialize rmp serde", times, || {
    //                 //     if let Some(bytes) = self.content.compiled_cache.lock().unwrap().get(&key) {
    //                 //         // let opcodes: Arc<Vec<Opcode>> = bincode::deserialize(bytes).expect("Failed to deserialize opcodes");;
    //                 //         let opcodes: Vec<Opcode> = rmp_serde::from_slice(bytes).expect("Failed to deserialize opcodes");;
    //                 //         // println!("Decompiled Expression {:?}", opcodes);
    //                 //     }
    //                 // });
    //             }
    //
    //         }
    //
    //     }
    //
    //
    // }

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
                compiled_cache: self.compiled_cache.clone(),
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
}

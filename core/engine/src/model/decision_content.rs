use std::sync::Arc;
use serde::{Deserialize, Serialize};
use zen_types::decision::{DecisionEdge, DecisionNode, DecisionNodeKind};
use ahash::{HashMap, HashMapExt};
use zen_expression::compiler::Opcode;
use zen_expression::{ExpressionKind, Isolate};

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
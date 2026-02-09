use ahash::HashMapExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zen_expression::{CompilationKey, ExpressionKind, Isolate, OpcodeCache};
use zen_types::decision::{DecisionEdge, DecisionNode, DecisionNodeKind};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DecisionContent {
    pub nodes: Vec<Arc<DecisionNode>>,
    pub edges: Vec<Arc<DecisionEdge>>,

    #[serde(skip)]
    pub compiled_cache: Option<Arc<OpcodeCache>>,
}

impl DecisionContent {
    pub fn compile(&mut self) {
        let mut sources: Vec<(Arc<str>, ExpressionKind)> = Vec::new();

        for node in &self.nodes {
            match &node.kind {
                DecisionNodeKind::ExpressionNode { content } => {
                    for expr in content.expressions.iter() {
                        if !expr.key.is_empty() && !expr.value.is_empty() {
                            sources.push((expr.value.clone(), ExpressionKind::Standard));
                        }
                    }
                }
                DecisionNodeKind::DecisionTableNode { content } => {
                    for rule in content.rules.iter() {
                        for input in content.inputs.iter() {
                            let Some(rule_value) = rule.get(&input.id) else {
                                continue;
                            };

                            let kind = if input.field.is_some() {
                                ExpressionKind::Unary
                            } else {
                                ExpressionKind::Standard
                            };

                            sources.push((rule_value.clone(), kind));
                        }

                        for output in content.outputs.iter() {
                            let Some(rule_value) = rule.get(&output.id) else {
                                continue;
                            };

                            sources.push((rule_value.clone(), ExpressionKind::Standard));
                        }
                    }
                }
                _ => {}
            }
        }

        let mut cache: OpcodeCache = OpcodeCache::new();
        let mut isolate = Isolate::new();

        for (source, kind) in &sources {
            let key = CompilationKey {
                kind: kind.clone(),
                source: source.clone(),
            };

            if cache.contains_key(&key) {
                continue;
            }

            let result = match kind {
                ExpressionKind::Standard => isolate
                    .compile_standard(source)
                    .map(|e| e.bytecode().to_vec()),
                ExpressionKind::Unary => {
                    isolate.compile_unary(source).map(|e| e.bytecode().to_vec())
                }
            };
            if let Ok(bytecode) = result {
                cache.insert(key, Arc::from(bytecode));
            }
        }

        self.compiled_cache.replace(Arc::new(cache));
    }
}

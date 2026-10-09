use super::GraphContent;
use std::sync::Arc;
use zen_expression::{ExpressionKind, Isolate, OpcodeCache};
use zen_types::decision::DecisionNodeKind;

impl GraphContent {
    pub(crate) fn build_opcode_cache(&mut self) {
        if self.compiled_cache.is_some() {
            return;
        }

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
            let map = match kind {
                ExpressionKind::Standard => &mut cache.standard,
                ExpressionKind::Unary => &mut cache.unary,
            };
            if map.contains_key(source) {
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
                map.insert(source.clone(), Arc::from(bytecode));
            }
        }

        self.compiled_cache.replace(Arc::new(cache));
    }
}

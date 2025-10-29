use crate::decision::CompilationKey;
use crate::model::ExpressionNodeContent;
use crate::nodes::context::{NodeContext, NodeContextExt};
use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use ahash::HashMap;
use std::rc::Rc;
use zen_expression::variable::{ToVariable, Variable};
use zen_expression::{ExpressionKind, Isolate};
use zen_types::decision::TransformAttributes;

#[derive(Debug, Clone)]
pub struct ExpressionNodeHandler;

pub type ExpressionNodeData = ExpressionNodeContent;
pub type ExpressionNodeTrace = HashMap<Rc<str>, ExpressionNodeTraceItem>;

impl NodeHandler for ExpressionNodeHandler {
    type NodeData = ExpressionNodeData;
    type TraceData = ExpressionNodeTrace;

    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        Some(ctx.node.transform_attributes.clone())
    }

    async fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        let result = Variable::empty_object();
        let mut isolate = Isolate::new();
        isolate.set_environment(ctx.input.depth_clone(1));

        for expression in ctx.node.expressions.iter() {
            if expression.key.is_empty() || expression.value.is_empty() {
                continue;
            }
            let key = CompilationKey {
                kind: ExpressionKind::Standard,
                source: expression.value.clone(),
            };
            let value;
            match ctx
                .extensions
                .compiled_cache
                .as_ref()
                .and_then(|cc| cc.get(&key))
            {
                Some(codes) => value = isolate.run_compiled(codes.as_slice()),
                None => value = isolate.run_standard(&expression.value),
            }

            let value = value.with_node_context(&ctx, |_| {
                format!(r#"Failed to evaluate expression: "{}""#, &expression.value)
            })?;
            ctx.trace(|trace| {
                trace.insert(
                    Rc::from(&*expression.key),
                    ExpressionNodeTraceItem {
                        result: value.clone(),
                    },
                );
            });

            isolate.update_environment(|env| {
                let Some(environment) = env else {
                    return;
                };

                let key = format!("$.{}", &expression.key);
                let _ = environment.dot_insert(key.as_str(), value.depth_clone(2));
            });

            result.dot_insert(&expression.key, value);
        }

        ctx.success(result)
    }
}

#[derive(Debug, Clone, ToVariable)]
pub struct ExpressionNodeTraceItem {
    result: Variable,
}

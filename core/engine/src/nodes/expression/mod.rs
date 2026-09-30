use crate::model::ExpressionNodeContent;
use crate::nodes::result::NodeResult;
use ahash::HashMap;
use bumpalo::Bump;
use std::rc::Rc;

use crate::nodes::context::{NodeContext, NodeContextExt};
use crate::nodes::definition::NodeHandler;
use zen_expression::lexer::{Identifier, Lexer, TokenKind};
use zen_expression::variable::{ToVariable, Variable};
use zen_types::decision::TransformAttributes;
use zen_types::symbol::Symbol;

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
        let mut isolate = ctx.isolate();
        let mut dollar_bound = false;

        for expression in ctx.node.expressions.iter() {
            if expression.key.is_empty() || expression.value.is_empty() {
                continue;
            }

            let value = isolate
                .run_standard(&expression.value)
                .with_node_context(&ctx, |_| {
                    format!(r#"Failed to evaluate expression: "{}""#, &expression.value)
                })?;
            let value = match &value {
                Variable::Object(_) | Variable::Array(_) if reads_context(&expression.value) => {
                    value.deep_clone()
                }
                _ => value,
            };
            ctx.trace(|trace| {
                trace.insert(
                    Rc::from(&*expression.key),
                    ExpressionNodeTraceItem {
                        result: value.clone(),
                    },
                );
            });

            insert_at_path(&result, &expression.key, value);
            if !dollar_bound {
                isolate.set_local(Variable::dollar_key(), result.shallow_clone());
                dollar_bound = true;
            }
        }

        ctx.success(result)
    }
}

fn reads_context(source: &str) -> bool {
    let bump = Bump::new();
    Lexer::new()
        .tokenize(&bump, source)
        .map(|tokens| {
            tokens.iter().any(|token| {
                matches!(
                    token.kind,
                    TokenKind::Identifier(Identifier::ContextReference | Identifier::RootReference)
                )
            })
        })
        .unwrap_or(true)
}

fn insert_at_path(root: &Variable, key: &str, value: Variable) {
    let mut parts = key.split('.');
    let Some(last) = parts.next_back() else {
        return;
    };

    let mut current = root.shallow_clone();
    for part in parts {
        let Variable::Object(object) = &current else {
            return;
        };

        let next = {
            let mut map = object.borrow_mut();
            match map.get_str(part).map(Variable::shallow_clone) {
                Some(Variable::Object(child)) if Rc::strong_count(&child) > 2 => {
                    let copy = Variable::Object(child).depth_clone(1);
                    map.insert(Symbol::from(part), copy.shallow_clone());
                    copy
                }
                Some(child @ Variable::Object(_)) => child,
                Some(_) => return,
                None => {
                    let created = Variable::empty_object();
                    map.insert(Symbol::from(part), created.shallow_clone());
                    created
                }
            }
        };
        current = next;
    }

    if let Variable::Object(object) = &current {
        object.borrow_mut().insert(Symbol::from(last), value);
    }
}

#[derive(Debug, Clone, ToVariable)]
pub struct ExpressionNodeTraceItem {
    result: Variable,
}

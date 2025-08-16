use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::{DecisionNodeKind, ExpressionNodeContent};
use ahash::{HashMap, HashMapExt};
use std::rc::Rc;
use std::sync::Arc;

use crate::handler::node::NodeError;
use anyhow::anyhow;
use serde::Serialize;
use tokio::sync::Mutex;
use zen_expression::variable::{ToVariable, Variable};
use zen_expression::{Isolate, ToVariable};

pub struct ExpressionHandler {
    trace: bool,
}

#[derive(Debug, Serialize, ToVariable)]
struct ExpressionTrace {
    result: Variable,
}

impl ExpressionHandler {
    pub fn new(trace: bool) -> Self {
        Self { trace }
    }

    pub async fn handle(&mut self, request: NodeRequest) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::ExpressionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let inner_handler_mutex = Arc::new(Mutex::new(ExpressionHandlerInner::new(self.trace)));

        content
            .transform_attributes
            .run_with(request.input, |input| {
                let inner_handler_mutex = inner_handler_mutex.clone();

                async move {
                    let mut inner_handler_ref = inner_handler_mutex.lock().await;
                    inner_handler_ref.handle(input, content).await
                }
            })
            .await
    }
}

struct ExpressionHandlerInner<'a> {
    isolate: Isolate<'a>,
    trace: bool,
}

impl<'a> ExpressionHandlerInner<'a> {
    pub fn new(trace: bool) -> Self {
        Self {
            isolate: Isolate::new(),
            trace,
        }
    }

    async fn handle(&mut self, input: Variable, content: &'a ExpressionNodeContent) -> NodeResult {
        let result = Variable::empty_object();
        let mut trace_map = self
            .trace
            .then(|| HashMap::<Rc<str>, ExpressionTrace>::new());

        self.isolate.set_environment(input.depth_clone(1));
        for expression in &content.expressions {
            if expression.key.is_empty() || expression.value.is_empty() {
                continue;
            }

            let value = self.isolate.run_standard(&expression.value).map_err(|_| {
                NodeError::PartialTrace {
                    trace: trace_map.as_ref().map(|v| v.to_variable()),
                    message: format!(r#"Failed to evaluate expression: "{}""#, &expression.value),
                }
            })?;
            if let Some(tmap) = &mut trace_map {
                tmap.insert(
                    Rc::from(expression.key.as_str()),
                    ExpressionTrace {
                        result: value.clone(),
                    },
                );
            }

            self.isolate.update_environment(|env| {
                let Some(environment) = env else {
                    return;
                };

                let key = format!("$.{}", &expression.key);
                let _ = environment.dot_insert(key.as_str(), value.depth_clone(2));
            });

            result.dot_insert(&expression.key, value);
        }

        Ok(NodeResponse {
            output: result,
            trace_data: trace_map.as_ref().map(|v| v.to_variable()),
        })
    }
}

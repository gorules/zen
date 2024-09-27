use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::DecisionNodeKind;
use ahash::{HashMap, HashMapExt};

use anyhow::{anyhow, Context};
use serde::Serialize;
use zen_expression::variable::Variable;
use zen_expression::Isolate;

pub struct ExpressionHandler<'a> {
    trace: bool,
    isolate: Isolate<'a>,
}

#[derive(Debug, Serialize)]
struct ExpressionTrace {
    result: String,
}

impl<'a> ExpressionHandler<'a> {
    pub fn new(trace: bool) -> Self {
        Self {
            trace,
            isolate: Isolate::new(),
        }
    }

    pub async fn handle(&mut self, request: &'a NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::ExpressionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let result = Variable::empty_object();
        let mut trace_map = self.trace.then(|| HashMap::<&str, ExpressionTrace>::new());

        self.isolate.set_environment(request.input.depth_clone(1));
        for expression in &content.expressions {
            let value = self.evaluate_expression(&expression.value)?;
            if let Some(tmap) = &mut trace_map {
                tmap.insert(
                    &expression.key,
                    ExpressionTrace {
                        result: serde_json::to_string(&value).unwrap_or("Error".to_owned()),
                    },
                );
            }

            self.isolate.update_environment(|env| {
                let Some(environment) = env else {
                    return;
                };

                let key = format!("$.{}", &expression.key);
                let _ = environment.dot_insert(key.as_str(), value.clone());
            });

            result.dot_insert(&expression.key, value);
        }

        Ok(NodeResponse {
            output: result,
            trace_data: trace_map
                .map(|tm| serde_json::to_value(tm))
                .transpose()
                .context("Failed to serialize trace data")?,
        })
    }

    fn evaluate_expression(&mut self, expression: &'a str) -> anyhow::Result<Variable> {
        self.isolate
            .run_standard(expression)
            .with_context(|| format!(r#"Failed to evaluate expression: "{expression}""#))
    }
}

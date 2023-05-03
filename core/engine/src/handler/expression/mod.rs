use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::model::DecisionNodeKind;
use crate::util::json_map::FlatJsonMap;
use anyhow::{anyhow, Context};
use serde_json::Value;
use zen_expression::isolate::Isolate;

pub struct ExpressionHandler<'a> {
    isolate: Isolate<'a>,
}

impl<'a> ExpressionHandler<'a> {
    pub fn new() -> Self {
        Self {
            isolate: Default::default(),
        }
    }

    pub async fn handle(&self, request: &'a NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::ExpressionNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        self.isolate.inject_env(&request.input);
        let mut result = FlatJsonMap::with_capacity(content.expressions.len());
        for expression in &content.expressions {
            let value = self.evaluate_expression(&expression.value)?;
            result.insert(&expression.key, value);
        }

        let output = result.to_json().context("Conversion to JSON failed")?;
        Ok(NodeResponse {
            output,
            trace_data: None,
        })
    }

    fn evaluate_expression(&self, expression: &'a str) -> anyhow::Result<Value> {
        self.isolate
            .run_standard(expression)
            .context("Failed to evaluate expression")
    }
}

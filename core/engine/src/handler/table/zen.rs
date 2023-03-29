use anyhow::{anyhow, Context};
use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::handler::table::{RowOutput, RowOutputKind};
use crate::model::decision::{DecisionNodeKind, DecisionTableContent, DecisionTableHitPolicy};
use zen_vm::isolate::Isolate;

#[derive(Debug, Serialize)]
struct RowResult {
    rule: Option<HashMap<String, String>>,
    index: usize,
    #[serde(skip)]
    output: RowOutput,
}

#[derive(Debug, Default)]
pub struct DecisionTableHandler<'a> {
    isolate: Isolate<'a>,
    trace: bool,
}

impl<'a> DecisionTableHandler<'a> {
    pub fn new(trace: bool) -> Self {
        Self {
            isolate: Default::default(),
            trace,
        }
    }

    pub async fn handle(&self, request: &'a NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::DecisionTableNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        self.isolate.inject_env(&request.input);

        match &content.hit_policy {
            DecisionTableHitPolicy::First => self.handle_first_hit(&content).await,
            DecisionTableHitPolicy::Collect => self.handle_collect(&content).await,
        }
    }

    async fn handle_first_hit(&self, content: &'a DecisionTableContent) -> NodeResult {
        for i in 0..content.rules.len() {
            if let Some(result) = self.evaluate_row(&content, i).await {
                return Ok(NodeResponse {
                    output: result.output.to_json().await?,
                    trace_data: match self.trace {
                        true => Some(
                            serde_json::to_value(result).context("Failed to parse trace data")?,
                        ),
                        false => None,
                    },
                });
            }
        }

        Ok(NodeResponse {
            output: Value::Null,
            trace_data: None,
        })
    }

    async fn handle_collect(&self, content: &'a DecisionTableContent) -> NodeResult {
        let mut results = Vec::new();
        for i in 0..content.rules.len() {
            if let Some(result) = self.evaluate_row(&content, i).await {
                results.push(result);
            }
        }

        let mut outputs = Vec::with_capacity(results.len());
        for res in &results {
            outputs.push(res.output.to_json().await?);
        }

        Ok(NodeResponse {
            output: serde_json::to_value(&outputs).context("Failed to parse table row output")?,
            trace_data: match self.trace {
                true => Some(serde_json::to_value(&results).context("Failed to parse trace data")?),
                false => None,
            },
        })
    }

    async fn evaluate_row(
        &self,
        content: &'a DecisionTableContent,
        index: usize,
    ) -> Option<RowResult> {
        let rule = content.rules.get(index)?;
        for input in &content.inputs {
            let rule_value = rule.get(input.id.as_str())?;
            if rule_value.is_empty() {
                continue;
            }

            if self.isolate.set_reference(input.field.as_str()).is_err() {
                return None;
            }

            let result = self.isolate.run(rule_value.as_str());
            if result.is_err() {
                return None;
            }

            let is_ok = result.unwrap().bool().unwrap_or_else(|_| false);
            if !is_ok {
                return None;
            }
        }

        let mut outputs: RowOutput = Default::default();
        for output in &content.outputs {
            let rule_value = rule.get(output.id.as_str())?;
            if rule_value.is_empty() {
                continue;
            }

            let res = self.isolate.run_standard(rule_value);
            if res.is_err() {
                return None;
            }

            outputs.insert(
                output.field.clone(),
                RowOutputKind::Value(res.unwrap().to_value().ok()?),
            );
        }

        if !self.trace {
            return Some(RowResult {
                output: outputs,
                rule: None,
                index,
            });
        }

        let rule_id = match rule.get("_id") {
            Some(rid) => rid.clone(),
            None => "".to_string(),
        };

        let mut expressions: HashMap<String, String> = Default::default();
        expressions.insert("_id".to_string(), rule_id.clone());

        for input in &content.inputs {
            let rule_value = rule.get(input.id.as_str())?;
            expressions.insert(input.field.clone(), rule_value.clone());
        }

        Some(RowResult {
            output: outputs,
            rule: Some(expressions),
            index,
        })
    }
}

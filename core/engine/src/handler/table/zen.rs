use ahash::HashMap;
use anyhow::anyhow;
use std::sync::Arc;

use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::handler::table::{RowOutput, RowOutputKind};
use crate::model::{DecisionNodeKind, DecisionTableContent, DecisionTableHitPolicy};
use serde::Serialize;
use tokio::sync::Mutex;
use zen_expression::variable::Variable;
use zen_expression::Isolate;

#[derive(Debug, Serialize)]
struct RowResult {
    rule: Option<HashMap<String, String>>,
    reference_map: Option<HashMap<String, Variable>>,
    index: usize,
    #[serde(skip)]
    output: RowOutput,
}

#[derive(Debug)]
pub struct DecisionTableHandler {
    trace: bool,
}

impl DecisionTableHandler {
    pub fn new(trace: bool) -> Self {
        Self { trace }
    }

    pub async fn handle(&mut self, request: NodeRequest) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::DecisionTableNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        let inner_handler = DecisionTableHandlerInner::new(self.trace);
        inner_handler
            .handle(request.input.depth_clone(1), content)
            .await
    }
}

struct DecisionTableHandlerInner<'a> {
    isolate: Isolate<'a>,
    trace: bool,
}

impl<'a> DecisionTableHandlerInner<'a> {
    pub fn new(trace: bool) -> Self {
        Self {
            isolate: Isolate::new(),
            trace,
        }
    }

    pub async fn handle(self, input: Variable, content: &'a DecisionTableContent) -> NodeResult {
        let self_mutex = Arc::new(Mutex::new(self));

        content
            .transform_attributes
            .run_with(input, |input| {
                let self_mutex = self_mutex.clone();
                async move {
                    let mut self_ref = self_mutex.lock().await;

                    self_ref.isolate.clear_references();
                    self_ref.isolate.set_environment(input);
                    match &content.hit_policy {
                        DecisionTableHitPolicy::First => self_ref.handle_first_hit(&content).await,
                        DecisionTableHitPolicy::Collect => self_ref.handle_collect(&content).await,
                    }
                }
            })
            .await
    }

    async fn handle_first_hit(&mut self, content: &'a DecisionTableContent) -> NodeResult {
        for i in 0..content.rules.len() {
            if let Some(result) = self.evaluate_row(&content, i) {
                return Ok(NodeResponse {
                    output: result.output.to_json().await,
                    trace_data: self
                        .trace
                        .then(|| serde_json::to_value(&result).ok())
                        .flatten(),
                });
            }
        }

        Ok(NodeResponse {
            output: Variable::Null,
            trace_data: None,
        })
    }

    async fn handle_collect(&mut self, content: &'a DecisionTableContent) -> NodeResult {
        let mut results = Vec::new();
        for i in 0..content.rules.len() {
            if let Some(result) = self.evaluate_row(&content, i) {
                results.push(result);
            }
        }

        let mut outputs = Vec::with_capacity(results.len());
        for res in &results {
            outputs.push(res.output.to_json().await);
        }

        Ok(NodeResponse {
            output: Variable::from_array(outputs),
            trace_data: self
                .trace
                .then(|| serde_json::to_value(&results).ok())
                .flatten(),
        })
    }

    fn evaluate_row(
        &mut self,
        content: &'a DecisionTableContent,
        index: usize,
    ) -> Option<RowResult> {
        let rule = content.rules.get(index)?;
        for input in &content.inputs {
            let rule_value = rule.get(input.id.as_str())?;
            if rule_value.trim().is_empty() {
                continue;
            }

            match &input.field {
                None => {
                    let result = self.isolate.run_standard(rule_value.as_str()).ok()?;
                    if !result.as_bool().unwrap_or(false) {
                        return None;
                    }
                }
                Some(field) => {
                    self.isolate.set_reference(field.as_str()).ok()?;
                    if !self.isolate.run_unary(rule_value.as_str()).ok()? {
                        return None;
                    }
                }
            }
        }

        let mut outputs: RowOutput = Default::default();
        for output in &content.outputs {
            let rule_value = rule.get(output.id.as_str())?;
            if rule_value.trim().is_empty() {
                continue;
            }

            let res = self.isolate.run_standard(rule_value).ok()?;
            outputs.push(&output.field, RowOutputKind::Variable(res));
        }

        if !self.trace {
            return Some(RowResult {
                output: outputs,
                rule: None,
                reference_map: None,
                index,
            });
        }

        let rule_id = match rule.get("_id") {
            Some(rid) => rid.clone(),
            None => "".to_string(),
        };

        let mut expressions: HashMap<String, String> = Default::default();
        let mut reference_map: HashMap<String, Variable> = Default::default();

        expressions.insert("_id".to_string(), rule_id.clone());
        if let Some(description) = rule.get("_description") {
            expressions.insert("_description".to_string(), description.clone());
        }

        for input in &content.inputs {
            let rule_value = rule.get(input.id.as_str())?;
            let Some(input_field) = &input.field else {
                continue;
            };

            if let Some(reference) = self.isolate.get_reference(input_field.as_str()) {
                reference_map.insert(input_field.clone(), reference);
            } else if let Some(reference) = self.isolate.run_standard(input_field.as_str()).ok() {
                reference_map.insert(input_field.clone(), reference);
            }

            let input_identifier = format!("{input_field}[{}]", &input.id);
            expressions.insert(input_identifier, rule_value.clone());
        }

        Some(RowResult {
            output: outputs,
            rule: Some(expressions),
            reference_map: Some(reference_map),
            index,
        })
    }
}

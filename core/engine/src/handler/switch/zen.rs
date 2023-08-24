use anyhow::{anyhow, Context};
use std::collections::HashMap;
use zen_expression::hashmap;

use serde::Serialize;
use serde_json::Value;

use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::handler::switch::{RuleOutput, RuleOutputKind};
use crate::model::{DecisionNodeKind, RuleValue, SwitchContent, SwitchHitPolicy};
use zen_expression::isolate::Isolate;

#[derive(Debug, Serialize)]
struct RuleResult {
    rule: Option<HashMap<String, String>>,
    reference_map: Option<HashMap<String, Value>>,
    index: usize,
    #[serde(skip)]
    output: RuleOutput,
}

#[derive(Debug, Default)]
pub struct SwitchHandler<'a> {
    isolate: Isolate<'a>,
    trace: bool,
}

impl<'a> SwitchHandler<'a> {
    pub fn new(trace: bool) -> Self {
        Self {
            isolate: Default::default(),
            trace,
        }
    }

    pub async fn handle(&self, request: &'a NodeRequest<'_>) -> NodeResult {
        let content = match &request.node.kind {
            DecisionNodeKind::SwitchNode { content } => Ok(content),
            _ => Err(anyhow!("Unexpected node type")),
        }?;

        self.isolate.inject_env(&request.input);

        match &content.hit_policy {
            SwitchHitPolicy::First => self.handle_first_hit(&content).await,
            SwitchHitPolicy::Collect => self.handle_collect(&content).await,
        }
    }

    async fn handle_first_hit(&self, content: &'a SwitchContent) -> NodeResult {
        // evaluate for each key value pair of rules
        println!("{:#?}", content);
        for (rule_key, sub_rule_or_model) in &content.rules {
            if let Some(result) = self.evaluate_rule(rule_key, sub_rule_or_model) {
                return Ok(NodeResponse {
                    output: result.output.to_json().await?,
                    trace_data: self
                        .trace
                        .then(|| {
                            serde_json::to_value(&result).context("Failed to parse trace data")
                        })
                        .transpose()?,
                });
            }
        }

        // If there's an "else" rule, evaluate it (assuming it's a terminal node)
        if let Some(RuleValue::Model(model)) = content.rules.get("else") {
            let mut outputs: RuleOutput = Default::default();
            outputs.push(
                &content.outputs[0].field,
                RuleOutputKind::Value(Value::String(model.clone())),
            );

            return Ok(NodeResponse {
                output: outputs.to_json().await?,
                trace_data: self
                    .trace
                    .then(|| {
                        serde_json::to_value(&RuleResult {
                            output: outputs,
                            rule: Some(hashmap! {String::from("else") => model.clone()}),
                            reference_map: None,
                            index: 0,
                        })
                        .context("Failed to parse trace data")
                    })
                    .transpose()?,
            });
        }

        Ok(NodeResponse {
            output: Value::Null,
            trace_data: None,
        })
    }

    async fn handle_collect(&self, content: &'a SwitchContent) -> NodeResult {
        unimplemented!();
    }

    fn evaluate_rule(&self, rule_key: &'a String, rule_value: &'a RuleValue) -> Option<RuleResult> {
        let rule = self.isolate.run_standard(rule_key.as_str()).ok()?;
        let is_rule_ok = rule.as_bool().unwrap_or(false);

        if is_rule_ok {
            match rule_value {
                // if not nested
                RuleValue::Model(model) => {
                    let mut outputs: RuleOutput = Default::default();
                    outputs.push(
                        rule_key,
                        RuleOutputKind::Value(Value::String(model.clone())),
                    );
                    let mut map = HashMap::new();
                    map.insert(rule_key.clone(), model.clone());
                    return Some(RuleResult {
                        output: outputs,
                        rule: Some(map),
                        reference_map: None,
                        index: 0,
                    });
                }
                RuleValue::Nested(nested_rules) => {
                    // Recursively evaluate the nested rules
                    for (nested_key, nested_value) in nested_rules {
                        if let Some(result) = self.evaluate_rule(nested_key, nested_value) {
                            return Some(result);
                        }
                    }

                    if let Some(RuleValue::Model(model)) = nested_rules.get("else") {
                        let mut outputs: RuleOutput = Default::default();
                        outputs.push(
                            rule_key,
                            RuleOutputKind::Value(Value::String(model.clone())),
                        );
                        let mut map = HashMap::new();
                        map.insert(rule_key.clone(), model.clone());
                        return Some(RuleResult {
                            output: outputs,
                            rule: Some(map),
                            reference_map: None,
                            index: 0,
                        });
                    }
                }
                _ => {}
            }
        }
        None
    }
}

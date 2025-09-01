use crate::nodes::definition::NodeHandler;
use crate::nodes::result::NodeResult;
use crate::nodes::NodeContext;
use ahash::HashMap;
use serde::Serialize;
use std::ops::Deref;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::variable::ToVariable;
use zen_expression::Isolate;
use zen_types::decision::{DecisionTableContent, DecisionTableHitPolicy, TransformAttributes};
use zen_types::variable::Variable;

pub struct DecisionTableNodeHandler;

pub type DecisionTableNodeData = DecisionTableContent;

type DecisionTableContext = NodeContext<DecisionTableNodeData, DecisionTableNodeTrace>;

impl NodeHandler for DecisionTableNodeHandler {
    type NodeData = DecisionTableNodeData;
    type TraceData = DecisionTableNodeTrace;

    fn transform_attributes(
        &self,
        ctx: &NodeContext<Self::NodeData, Self::TraceData>,
    ) -> Option<TransformAttributes> {
        Some(ctx.node.transform_attributes.clone())
    }

    fn handle(&self, ctx: NodeContext<Self::NodeData, Self::TraceData>) -> NodeResult {
        match ctx.node.hit_policy {
            DecisionTableHitPolicy::First => self.handle_first_hit(ctx),
            DecisionTableHitPolicy::Collect => self.handle_collect(ctx),
        }
    }
}

impl DecisionTableNodeHandler {
    fn handle_first_hit(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut isolate = Isolate::new();

        for (index, rule) in ctx.node.rules.iter().enumerate() {
            if let Some(result) = self.evaluate_row(&ctx, rule, &mut isolate) {
                return match result {
                    RowResult::Output(output) => ctx.success(output),
                    RowResult::WithTrace {
                        output,
                        reference_map,
                        rule,
                    } => {
                        ctx.trace(|t| {
                            *t = DecisionTableNodeTrace::FirstHit(DecisionTableRowTrace {
                                reference_map,
                                index,
                                rule,
                            })
                        });

                        ctx.success(output)
                    }
                };
            }
        }

        ctx.success(Variable::Null)
    }

    fn handle_collect(&self, ctx: DecisionTableContext) -> NodeResult {
        let mut isolate = Isolate::new();
        let mut outputs = Vec::new();
        let mut traces = Vec::new();

        for (index, rule) in ctx.node.rules.iter().enumerate() {
            if let Some(result) = self.evaluate_row(&ctx, rule, &mut isolate) {
                match result {
                    RowResult::Output(output) => {
                        outputs.push(output);
                    }
                    RowResult::WithTrace {
                        output,
                        reference_map,
                        rule,
                    } => {
                        outputs.push(output);
                        traces.push(DecisionTableRowTrace {
                            index,
                            rule,
                            reference_map,
                        });
                    }
                }
            }
        }

        ctx.trace(|t| {
            *t = DecisionTableNodeTrace::Collect(traces);
        });

        ctx.success(Variable::from_array(outputs))
    }

    fn evaluate_row<'a>(
        &self,
        ctx: &'a DecisionTableContext,
        rule: &'a HashMap<Arc<str>, Arc<str>>,
        isolate: &mut Isolate<'a>,
    ) -> Option<RowResult> {
        let content = &ctx.node;
        for input in &content.inputs {
            let rule_value = rule.get(&input.id)?;
            if rule_value.trim().is_empty() {
                continue;
            }

            match &input.field {
                None => {
                    let result = isolate.run_standard(rule_value).ok()?;
                    if !result.as_bool().unwrap_or(false) {
                        return None;
                    }
                }
                Some(field) => {
                    isolate.set_reference(&field).ok()?;
                    if !isolate.run_unary(&rule_value).ok()? {
                        return None;
                    }
                }
            }
        }

        let mut outputs: HashMap<Rc<str>, Variable> = Default::default();
        for output in &content.outputs {
            let rule_value = rule.get(&output.id)?;
            if rule_value.trim().is_empty() {
                continue;
            }

            let res = isolate.run_standard(rule_value).ok()?;
            outputs.insert(Rc::from(&*output.field), res);
        }

        if !ctx.has_trace() {
            return Some(RowResult::Output(outputs.to_variable()));
        }

        let id_str = Rc::<str>::from("_id");
        let description_str = Rc::<str>::from("_description");

        let rule_id = match rule.get(id_str.as_ref()) {
            Some(rid) => Rc::<str>::from(rid.deref()),
            None => Rc::from(""),
        };

        let mut expressions: HashMap<Rc<str>, Rc<str>> = Default::default();
        let mut reference_map: HashMap<Rc<str>, Variable> = Default::default();

        expressions.insert(id_str.clone(), rule_id.clone());
        if let Some(description) = rule.get(description_str.as_ref()) {
            expressions.insert(description_str.clone(), Rc::from(description.deref()));
        }

        for input in &content.inputs {
            let rule_value = rule.get(input.id.deref())?;
            let Some(input_field) = &input.field else {
                continue;
            };

            if let Some(reference) = isolate.get_reference(input_field.deref()) {
                reference_map.insert(Rc::from(input_field.deref()), reference);
            } else if let Some(reference) = isolate.run_standard(input_field.deref()).ok() {
                reference_map.insert(Rc::from(input_field.deref()), reference);
            }

            let input_identifier = format!("{input_field}[{}]", &input.id);
            expressions.insert(
                Rc::from(input_identifier.as_str()),
                Rc::from(rule_value.deref()),
            );
        }

        Some(RowResult::WithTrace {
            output: outputs.to_variable(),
            reference_map,
            rule: expressions,
        })
    }
}

enum RowResult {
    Output(Variable),
    WithTrace {
        output: Variable,
        reference_map: HashMap<Rc<str>, Variable>,
        rule: HashMap<Rc<str>, Rc<str>>,
    },
}

#[derive(Debug, Clone, Serialize, ToVariable)]
pub struct DecisionTableRowTrace {
    index: usize,
    reference_map: HashMap<Rc<str>, Variable>,
    rule: HashMap<Rc<str>, Rc<str>>,
}

#[derive(Debug, Clone, Serialize, ToVariable)]
#[serde(untagged)]
pub enum DecisionTableNodeTrace {
    FirstHit(DecisionTableRowTrace),
    Collect(Vec<DecisionTableRowTrace>),
}

impl Default for DecisionTableNodeTrace {
    fn default() -> Self {
        DecisionTableNodeTrace::Collect(Default::default())
    }
}

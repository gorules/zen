use crate::model::{TransformAttributes, TransformExecutionMode};
use crate::nodes::result::NodeResult;
use crate::nodes::{NodeContextBase, NodeContextExt};
use std::future::Future;
use std::ops::Deref;
use zen_expression::{Isolate, Variable};

pub(crate) trait TransformAttributesExecution {
    async fn run_with<F, Fut>(&self, ctx: NodeContextBase, evaluate: F) -> NodeResult
    where
        F: Fn(Variable, bool) -> Fut,
        Fut: Future<Output = NodeResult>;
}

impl TransformAttributesExecution for TransformAttributes {
    async fn run_with<F, Fut>(&self, ctx: NodeContextBase, evaluate: F) -> NodeResult
    where
        F: Fn(Variable, bool) -> Fut,
        Fut: Future<Output = NodeResult>,
    {
        let input = match &self.input_field {
            None => ctx.input.clone(),
            Some(input_field) => {
                let mut isolate = Isolate::new();
                isolate.set_environment(ctx.input.clone());
                let calculated_input = isolate
                    .run_standard(input_field.deref())
                    .node_context_message(&ctx, "Failed to evaluate expression")?;

                let nodes = ctx.input.dot("$nodes").unwrap_or(Variable::Null);
                let params = ctx.input.dot("$params").unwrap_or(Variable::Null);

                match &calculated_input {
                    Variable::Array(arr) => {
                        let arr = arr.borrow();
                        let s: Vec<_> = arr
                            .iter()
                            .map(|v| {
                                let new_v = v.depth_clone(1);
                                new_v.dot_insert("$nodes", nodes.clone());
                                new_v.dot_insert("$params", params.clone());
                                new_v
                            })
                            .collect();

                        Variable::from_array(s)
                    }
                    _ => {
                        let new_input = calculated_input.depth_clone(1);
                        new_input.dot_insert("$nodes", nodes);
                        new_input.dot_insert("$params", params);
                        new_input
                    }
                }
            }
        };

        // let mut trace_data: Option<Variable> = None;
        let mut output = match self.execution_mode {
            TransformExecutionMode::Single => {
                let response = evaluate(input, false).await?;
                if let Some(td) = response.trace_data {
                    ctx.trace(|t| {
                        *t = td;
                    });
                }

                response.output.dot_remove("$nodes");
                response.output.dot_remove("$params");
                response.output
            }
            TransformExecutionMode::Loop => {
                let input_array_ref = input
                    .as_array()
                    .node_context_message(&ctx, "Expected an array")?;
                let input_array = input_array_ref.borrow();
                ctx.trace(|t| {
                    *t = Variable::from_array(Vec::with_capacity(input_array.len()));
                });

                let mut output_array = Vec::with_capacity(input_array.len());
                for (index, input) in input_array.iter().enumerate() {
                    let has_more = index < input_array.len() - 1;
                    let mut response = evaluate(input.clone(), has_more).await?;
                    if let Some(td) = response.trace_data {
                        ctx.trace(|var| {
                            if let Variable::Array(arr) = var {
                                let mut arr_mut = arr.borrow_mut();
                                arr_mut.push(td);
                            };
                        });
                    }

                    if self.pass_through {
                        response.output = input.clone().merge_clone(&response.output);
                    }

                    response.output.dot_remove("$nodes");
                    response.output.dot_remove("$params");
                    output_array.push(response.output);
                }

                Variable::from_array(output_array)
            }
        };

        if let Some(output_path) = &self.output_path {
            let new_output = Variable::empty_object();
            new_output.dot_insert(output_path.deref(), output);

            output = new_output;
        }

        if self.pass_through {
            let mut node_input = ctx.input.clone();
            output = node_input.merge_clone(&output)
        }

        ctx.success(output)
    }
}

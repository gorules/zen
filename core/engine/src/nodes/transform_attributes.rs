use crate::model::{TransformAttributes, TransformExecutionMode};
use crate::nodes::result::{NodeResponse, NodeResult};
use crate::nodes::{NodeContextBase, NodeContextExt};
use std::future::Future;
use std::ops::Deref;
use zen_expression::{Isolate, Variable};

pub trait TransformAttributesExecution {
    async fn run_with<F, Fut>(&self, ctx: NodeContextBase, evaluate: F) -> NodeResult
    where
        F: Fn(Variable) -> Fut,
        Fut: Future<Output = NodeResult>;
}

impl TransformAttributesExecution for TransformAttributes {
    async fn run_with<F, Fut>(&self, ctx: NodeContextBase, evaluate: F) -> NodeResult
    where
        F: Fn(Variable) -> Fut,
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
                match &calculated_input {
                    Variable::Array(arr) => {
                        let arr = arr.borrow();
                        let s: Vec<_> = arr
                            .iter()
                            .map(|v| {
                                let new_v = v.depth_clone(1);
                                new_v.dot_insert("$nodes", nodes.clone());
                                new_v
                            })
                            .collect();

                        Variable::from_array(s)
                    }
                    _ => {
                        let new_input = calculated_input.depth_clone(1);
                        new_input.dot_insert("$nodes", nodes);
                        new_input
                    }
                }
            }
        };

        let mut trace_data: Option<Variable> = None;
        let mut output = match self.execution_mode {
            TransformExecutionMode::Single => {
                let response = evaluate(input).await?;
                if let Some(td) = response.trace_data {
                    trace_data.replace(td);
                }

                response.output.dot_remove("$nodes");
                response.output
            }
            TransformExecutionMode::Loop => {
                let input_array_ref = input
                    .as_array()
                    .node_context_message(&ctx, "Expected an array")?;
                let input_array = input_array_ref.borrow();

                let mut output_array = Vec::with_capacity(input_array.len());
                let mut trace_datum = Vec::with_capacity(input_array.len());
                for input in input_array.iter() {
                    let mut response = evaluate(input.clone()).await?;
                    if let Some(td) = response.trace_data {
                        trace_datum.push(td);
                    }

                    if self.pass_through {
                        response.output = input.clone().merge_clone(&response.output);
                    }

                    response.output.dot_remove("$nodes");
                    output_array.push(response.output);
                }

                trace_data.replace(Variable::from_array(trace_datum));
                Variable::from_array(output_array)
            }
        };

        if let Some(output_path) = &self.output_path {
            let new_output = Variable::empty_object();
            new_output.dot_insert(output_path.deref(), output);

            output = new_output;
        }

        if self.pass_through {
            let mut node_input = ctx.input;
            output = node_input.merge_clone(&output)
        }

        Ok(NodeResponse { output, trace_data })
    }
}

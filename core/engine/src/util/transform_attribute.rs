use crate::handler::node::{NodeResponse, NodeResult};
use crate::model::{TransformAttributes, TransformExecutionMode};
use anyhow::Context;
use serde_json::Value;
use std::future::Future;
use zen_expression::{Isolate, Variable};

impl TransformAttributes {
    pub(crate) async fn run_with<F, Fut>(&self, input: Variable, evaluate: F) -> NodeResult
    where
        F: Fn(Variable) -> Fut,
        Fut: Future<Output = NodeResult>,
    {
        let input = match &self.input_field {
            None => input.clone(),
            Some(input_field) => {
                let mut isolate = Isolate::new();
                isolate.set_environment(input.clone());
                isolate.run_standard(input_field.as_str())?
            }
        };

        let mut trace_data: Option<Value> = None;
        let mut output = match self.execution_mode {
            TransformExecutionMode::Single => {
                let response = evaluate(input).await?;
                if let Some(td) = response.trace_data {
                    trace_data.replace(td);
                }

                response.output
            }
            TransformExecutionMode::Loop => {
                let input_array_ref = input.as_array().context("Expected an array")?;
                let input_array = input_array_ref.borrow();

                let mut output_array = Vec::with_capacity(input_array.len());
                let mut trace_datum = Vec::with_capacity(input_array.len());
                for input in input_array.iter() {
                    let response = evaluate(input.clone()).await?;

                    output_array.push(response.output);
                    if let Some(td) = response.trace_data {
                        trace_datum.push(td);
                    }
                }

                trace_data.replace(Value::Array(trace_datum));
                Variable::from_array(output_array)
            }
        };

        if let Some(output_path) = &self.output_path {
            let new_output = Variable::empty_object();
            new_output.dot_insert(output_path.as_str(), output);

            output = new_output;
        }

        Ok(NodeResponse { output, trace_data })
    }
}

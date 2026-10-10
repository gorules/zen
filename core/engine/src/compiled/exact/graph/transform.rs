use super::Row;
use crate::nodes::{NodeError, NodeResponse, NodeResult};
use std::future::Future;
use std::ops::Deref;
use zen_expression::Variable;
use zen_types::decision::{TransformAttributes, TransformExecutionMode};

impl Row<'_> {
    pub(crate) async fn transform<F, Fut>(&self, attributes: &TransformAttributes, evaluate: F) -> NodeResult
    where
        F: Fn(Variable, bool) -> Fut,
        Fut: Future<Output = NodeResult>,
    {
        let mut trace = self.trace.then_some(Variable::Null);
        let input = match &attributes.input_field {
            None => self.input.clone(),
            Some(input_field) => {
                let mut session = self.session();
                session
                    .run_standard(input_field.deref())
                    .map_err(|err| self.fault(&trace, format!("Failed to evaluate expression: {err}")))?
            }
        };

        let mut output = match attributes.execution_mode {
            TransformExecutionMode::Single => {
                let response = evaluate(input, false).await?;
                if let Some(td) = response.trace_data {
                    if let Some(t) = &mut trace {
                        *t = td;
                    }
                }

                response.output
            }
            TransformExecutionMode::Loop => {
                let input_array_ref = input
                    .as_array()
                    .ok_or_else(|| self.fault(&trace, "Expected an array".to_string()))?;
                let input_array = input_array_ref.borrow().clone();
                if let Some(t) = &mut trace {
                    *t = Variable::from_array(Vec::with_capacity(input_array.len()));
                }

                let mut output_array = Vec::with_capacity(input_array.len());
                for (index, input) in input_array.iter().enumerate() {
                    let has_more = index < input_array.len() - 1;
                    let mut response = evaluate(input.clone(), has_more).await?;
                    if let Some(td) = response.trace_data {
                        if let Some(Variable::Array(arr)) = &trace {
                            arr.borrow_mut().push(td);
                        }
                    }

                    if attributes.pass_through {
                        response.output = input.clone().merge_clone(&response.output);
                    }

                    output_array.push(response.output);
                }

                Variable::from_array(output_array)
            }
        };

        if let Some(output_path) = &attributes.output_path {
            let new_output = Variable::empty_object();
            new_output.dot_insert(output_path.deref(), output);

            output = new_output;
        }

        if attributes.pass_through {
            let mut node_input = self.input.clone();
            output = node_input.merge_clone(&output)
        }

        Ok(NodeResponse {
            output,
            trace_data: trace,
        })
    }

    fn fault(&self, trace: &Option<Variable>, message: String) -> NodeError {
        NodeError {
            node_id: self.id.clone(),
            trace: trace.clone(),
            source: message.into(),
        }
    }
}

use super::Row;
use crate::compiled::host::{NodeResponse, NodeResult};
use crate::compiled::trace::collect::Fact;
use std::future::Future;
use std::ops::Deref;
use zen_expression::clock::EvalContext;
use zen_expression::Variable;
use zen_types::decision::{TransformAttributes, TransformExecutionMode};

impl Row<'_> {
    pub(crate) async fn transform<F, Fut>(
        &self,
        attributes: &TransformAttributes,
        evaluate: F,
    ) -> NodeResult
    where
        F: Fn(Variable, bool, EvalContext) -> Fut,
        Fut: Future<Output = NodeResult>,
    {
        let input = match &attributes.input_field {
            None => self.input.clone(),
            Some(input_field) => {
                let mut session = self.session();
                session
                    .run_standard(input_field.deref())
                    .map_err(|err| self.error(crate::compiled::exact::Failure::input(err)))?
            }
        };

        let output = match attributes.execution_mode {
            TransformExecutionMode::Single => {
                let response = evaluate(input, false, self.eval.clone()).await?;
                if let Some(td) = response.trace_data {
                    self.push(Fact::Data(td));
                }

                response.output
            }
            TransformExecutionMode::Loop => {
                let input_array_ref = input
                    .as_array()
                    .ok_or_else(|| self.error("Expected an array".to_string()))?;
                let input_array = input_array_ref.borrow().clone();
                let mut items: Option<Vec<Fact>> =
                    self.traced().then(|| Vec::with_capacity(input_array.len()));

                let mut output_array = Vec::with_capacity(input_array.len());
                for (index, input) in input_array.iter().enumerate() {
                    let has_more = index < input_array.len() - 1;
                    let response =
                        Box::pin(evaluate(input.clone(), has_more, self.eval.item(index))).await?;
                    if let Some(items) = items.as_mut() {
                        let fact = match (self.take(), response.trace_data) {
                            (Fact::None, Some(td)) => Fact::Data(td),
                            (fact, _) => fact,
                        };
                        items.push(fact);
                    }

                    output_array.push(crate::compiled::exact::Attributes::item(
                        attributes.pass_through,
                        input,
                        response.output,
                    ));
                }

                if let Some(items) = items {
                    self.push(Fact::Items(items));
                }
                Variable::from_array(output_array)
            }
        };

        let output = crate::compiled::exact::Attributes::finish(
            attributes.output_path.as_deref(),
            attributes.pass_through,
            &self.input,
            output,
        );

        Ok(NodeResponse {
            output,
            trace_data: None,
        })
    }
}

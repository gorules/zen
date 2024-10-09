use crate::handler::custom_node_adapter::CustomNodeAdapter;
use crate::handler::function::function::Function;
use crate::handler::graph::{DecisionGraph, DecisionGraphConfig};
use crate::handler::node::{NodeRequest, NodeResponse, NodeResult};
use crate::loader::DecisionLoader;
use crate::model::{DecisionNodeKind, TransformExecutionMode};
use anyhow::{anyhow, Context};
use serde_json::Value;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::{Isolate, Variable};

pub struct DecisionHandler<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> {
    trace: bool,
    loader: Arc<L>,
    adapter: Arc<A>,
    max_depth: u8,
    js_function: Option<Rc<Function>>,
}

impl<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> DecisionHandler<L, A> {
    pub fn new(
        trace: bool,
        max_depth: u8,
        loader: Arc<L>,
        adapter: Arc<A>,
        js_function: Option<Rc<Function>>,
    ) -> Self {
        Self {
            trace,
            loader,
            adapter,
            max_depth,
            js_function,
        }
    }

    pub fn handle<'s, 'arg, 'recursion>(
        &'s self,
        request: &'arg NodeRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = NodeResult> + 'recursion>>
    where
        's: 'recursion,
        'arg: 'recursion,
    {
        Box::pin(async move {
            let content = match &request.node.kind {
                DecisionNodeKind::DecisionNode { content } => Ok(content),
                _ => Err(anyhow!("Unexpected node type")),
            }?;

            let mut isolate = Isolate::new();

            let sub_decision = self.loader.load(&content.key).await?;
            let mut sub_tree = DecisionGraph::try_new(DecisionGraphConfig {
                content: sub_decision.deref(),
                max_depth: self.max_depth,
                loader: self.loader.clone(),
                adapter: self.adapter.clone(),
                iteration: request.iteration + 1,
                trace: self.trace,
            })?
            .with_function(self.js_function.clone());

            let input_data = match &content.transform_attributes.input_field {
                None => request.input.clone(),
                Some(input_field) => {
                    isolate.set_environment(request.input.clone());
                    isolate.run_standard(input_field.as_str())?
                }
            };

            let mut trace_data: Option<Value> = None;
            let mut output_data = match &content.transform_attributes.execution_mode {
                TransformExecutionMode::Single => {
                    let response = sub_tree
                        .evaluate(request.input.clone())
                        .await
                        .map_err(|e| e.source)?;

                    if self.trace {
                        trace_data.replace(
                            serde_json::to_value(response.trace)
                                .context("Failed to serialize trace")?,
                        );
                    }

                    response.result
                }
                TransformExecutionMode::Loop => {
                    let input_array_ref = input_data.as_array().context("Expected an array")?;
                    let input_array = input_array_ref.borrow();

                    let mut output_array = Vec::with_capacity(input_array.len());
                    let mut trace_datum = Vec::with_capacity(input_array.len());
                    for input in input_array.iter() {
                        let response = sub_tree
                            .evaluate(input.clone())
                            .await
                            .map_err(|e| e.source)?;

                        output_array.push(response.result);
                        trace_datum.push(response.trace);
                    }

                    if self.trace {
                        trace_data.replace(
                            serde_json::to_value(trace_datum)
                                .context("Failed to parse trace data")?,
                        );
                    }

                    Variable::from_array(output_array)
                }
            };

            if let Some(output_path) = &content.transform_attributes.output_path {
                let new_output_data = Variable::empty_object();
                new_output_data.dot_insert(output_path.as_str(), output_data);

                output_data = new_output_data;
            }

            Ok(NodeResponse {
                output: output_data,
                trace_data,
            })
        })
    }
}

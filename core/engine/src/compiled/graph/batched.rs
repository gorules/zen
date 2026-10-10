use super::call::{Call, Load};
use super::{CompiledGraph, Nesting, Source, Step};
use crate::loader::LoaderError;
use crate::model::GraphContent;
use crate::nodes::custom::{CustomDecisionNode, CustomNodeRequest};
use crate::nodes::{NodeError, NodeHandlerExtensions};
use crate::EvaluationError;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;
use zen_expression::Variable;
use zen_types::decision::{CustomNodeContent, DecisionNodeContent, TransformAttributes, TransformExecutionMode};

pub(crate) enum Failure {
    Loader(LoaderError),
    Kind(&'static str),
    Compiled(Arc<GraphContent>),
}

impl Failure {
    fn copy(&self, key: &str) -> Option<Box<dyn Error>> {
        match self {
            Failure::Loader(LoaderError::NotFound(missing)) => Some(LoaderError::NotFound(missing.clone()).into()),
            Failure::Loader(LoaderError::Internal { .. }) => None,
            Failure::Kind(kind) => Some(Call::kind(key, kind)),
            Failure::Compiled(graph) => Call::compiled(graph).err(),
        }
    }

    fn source(self, key: &str) -> Box<dyn Error> {
        match self {
            Failure::Loader(error) => error.into(),
            Failure::Kind(kind) => Call::kind(key, kind),
            Failure::Compiled(graph) => match Call::compiled(&graph) {
                Err(source) => source,
                Ok(_) => Call::uncompiled(),
            },
        }
    }

    pub(super) fn shared(self, step: &Step, key: &str) -> Rc<EvaluationError> {
        Rc::new(EvaluationError::NodeError {
            node_id: step.node.id.clone(),
            source: self.source(key),
            trace: None,
        })
    }

    fn errors(&self, step: &Step, key: &str, count: usize) -> Option<Vec<Result<Variable, NodeError>>> {
        (0..count)
            .map(|_| {
                self.copy(key).map(|source| {
                    Err(NodeError {
                        node_id: step.node.id.clone(),
                        trace: None,
                        source,
                    })
                })
            })
            .collect()
    }
}

impl CompiledGraph {
    pub(super) fn batchable(attributes: &TransformAttributes, count: usize) -> bool {
        count >= 2 && attributes.input_field.is_none() && matches!(attributes.execution_mode, TransformExecutionMode::Single)
    }

    pub(super) async fn resolve(content: &DecisionNodeContent, extensions: &NodeHandlerExtensions) -> Result<Arc<GraphContent>, Failure> {
        let call = Call {
            key: &content.key,
            extensions,
            max_depth: 0,
            iteration: 0,
        };
        match call.load().await {
            Err(error) => Err(Failure::Loader(error)),
            Ok(Load::Kind(kind)) => Err(Failure::Kind(kind)),
            Ok(Load::Graph(graph)) => match Call::compiled(&graph) {
                Ok(_) => Ok(graph),
                Err(_) => Err(Failure::Compiled(graph)),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn batch(
        step: &Step,
        content: &DecisionNodeContent,
        graph: &Arc<GraphContent>,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
    ) -> Vec<Result<Variable, NodeError>> {
        let fault = |source: Box<dyn Error>| NodeError {
            node_id: step.node.id.clone(),
            trace: None,
            source,
        };
        let compiled = match Call::compiled(graph) {
            Ok(compiled) => compiled,
            Err(_) => {
                return inputs
                    .iter()
                    .map(|_| Err(fault(Call::compiled(graph).err().unwrap_or_else(Call::uncompiled))))
                    .collect()
            }
        };
        if iteration.saturating_add(1) >= max_depth {
            return inputs.iter().map(|_| Err(fault(EvaluationError::DepthLimitExceeded.to_string().into()))).collect();
        }
        let call = Call {
            key: &content.key,
            extensions,
            max_depth,
            iteration,
        };
        let attributes = &content.transform_attributes;
        let content = compiled.schemas(graph, extensions.loader()).await;
        let extensions = NodeHandlerExtensions {
            function_runtime: Default::default(),
            ..call.sub_extensions(&content)
        };
        let parents: Vec<Option<Variable>> = (0..inputs.len()).map(|row| nodes.get(row).cloned().flatten()).collect();
        let nesting = Nesting {
            parents: Some(&parents),
            iteration: iteration + 1,
        };
        let state = Box::pin(compiled.run(&content, &extensions, max_depth, &Source::Rows(inputs), inputs.len(), false, nesting)).await;
        state
            .errors
            .into_iter()
            .zip(state.results)
            .zip(inputs)
            .map(|((error, result), input)| match error {
                Some(error) => Err(fault(error.to_string().into())),
                None => {
                    let mut output = result.unwrap_or_else(Variable::empty_object);
                    if let Some(path) = &attributes.output_path {
                        let wrapped = Variable::empty_object();
                        wrapped.dot_insert(path.as_ref(), output);
                        output = wrapped;
                    }
                    if attributes.pass_through {
                        let mut node_input = input.clone();
                        output = node_input.merge_clone(&output);
                    }
                    Ok(output)
                }
            })
            .collect()
    }

    pub(super) async fn nested(
        step: &Step,
        content: &DecisionNodeContent,
        inputs: &[Variable],
        nodes: &[Option<Variable>],
        extensions: &NodeHandlerExtensions,
        max_depth: u8,
        iteration: u8,
    ) -> Option<Vec<Result<Variable, NodeError>>> {
        let attributes = &content.transform_attributes;
        if !Self::batchable(attributes, inputs.len()) {
            return None;
        }
        match Self::resolve(content, extensions).await {
            Ok(graph) => Some(Self::batch(step, content, &graph, inputs, nodes, extensions, max_depth, iteration).await),
            Err(failure) => failure.errors(step, &content.key, inputs.len()),
        }
    }

    pub(super) async fn custom(
        step: &Step,
        content: &CustomNodeContent,
        inputs: Vec<Variable>,
        nodes: Vec<Option<Variable>>,
        extensions: &NodeHandlerExtensions,
    ) -> Vec<Result<Variable, NodeError>> {
        let adapter = extensions.custom_node();
        let node = CustomDecisionNode {
            id: step.node.id.clone(),
            name: step.node.name.clone(),
            kind: content.kind.clone(),
            config: content.config.clone(),
        };
        let mut nodes = nodes.into_iter();
        let mut results = Vec::with_capacity(inputs.len());
        for input in inputs {
            let request = CustomNodeRequest {
                input: Self::with_nodes(input, nodes.next().flatten()),
                node: node.clone(),
            };
            results.push(adapter.handle(request).await.map(|response| response.output));
        }
        results
    }

    fn with_nodes(input: Variable, nodes: Option<Variable>) -> Variable {
        let (Some(nodes), Variable::Object(object)) = (nodes, &input) else {
            return input;
        };
        if Rc::strong_count(object) == 1 && Rc::weak_count(object) == 0 {
            object.borrow_mut().insert(Variable::nodes_key(), nodes);
            return input;
        }
        let mut map = object.borrow().clone();
        map.insert(Variable::nodes_key(), nodes);
        Variable::from_object(map)
    }
}

use crate::handler::custom_node_adapter::{CustomNodeAdapter, CustomNodeRequest};
use crate::handler::decision::DecisionHandler;
use crate::handler::expression::ExpressionHandler;
use crate::handler::function::function::{Function, FunctionConfig};
use crate::handler::function::module::console::ConsoleListener;
use crate::handler::function::module::zen::ZenListener;
use crate::handler::function::FunctionHandler;
use crate::handler::function_v1;
use crate::handler::function_v1::runtime::create_runtime;
use crate::handler::node::{NodeRequest, PartialTraceError};
use crate::handler::table::zen::DecisionTableHandler;
use crate::handler::traversal::{GraphWalker, StableDiDecisionGraph};
use crate::loader::DecisionLoader;
use crate::model::{DecisionContent, DecisionNodeKind, FunctionNodeContent};
use crate::util::validator_cache::ValidatorCache;
use crate::{EvaluationError, NodeError};
use ahash::{HashMap, HashMapExt};
use anyhow::anyhow;
use petgraph::algo::is_cyclic_directed;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use zen_expression::variable::Variable;

pub struct DecisionGraph<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> {
    initial_graph: StableDiDecisionGraph,
    graph: StableDiDecisionGraph,
    adapter: Arc<A>,
    loader: Arc<L>,
    trace: bool,
    max_depth: u8,
    iteration: u8,
    runtime: Option<Rc<Function>>,
    validator_cache: ValidatorCache,
}

pub struct DecisionGraphConfig<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> {
    pub loader: Arc<L>,
    pub adapter: Arc<A>,
    pub content: Arc<DecisionContent>,
    pub trace: bool,
    pub iteration: u8,
    pub max_depth: u8,
    pub validator_cache: Option<ValidatorCache>,
}

impl<L: DecisionLoader + 'static, A: CustomNodeAdapter + 'static> DecisionGraph<L, A> {
    pub fn try_new(
        config: DecisionGraphConfig<L, A>,
    ) -> Result<Self, DecisionGraphValidationError> {
        let content = config.content;
        let mut graph = StableDiDecisionGraph::new();
        let mut index_map = HashMap::new();

        for node in &content.nodes {
            let node_id = node.id.clone();
            let node_index = graph.add_node(node.clone());

            index_map.insert(node_id, node_index);
        }

        for (_, edge) in content.edges.iter().enumerate() {
            let source_index = index_map.get(&edge.source_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.source_id.to_string())
            })?;

            let target_index = index_map.get(&edge.target_id).ok_or_else(|| {
                DecisionGraphValidationError::MissingNode(edge.target_id.to_string())
            })?;

            graph.add_edge(source_index.clone(), target_index.clone(), edge.clone());
        }

        Ok(Self {
            initial_graph: graph.clone(),
            graph,
            iteration: config.iteration,
            trace: config.trace,
            loader: config.loader,
            adapter: config.adapter,
            max_depth: config.max_depth,
            validator_cache: config.validator_cache.unwrap_or_default(),
            runtime: None,
        })
    }

    pub(crate) fn with_function(mut self, runtime: Option<Rc<Function>>) -> Self {
        self.runtime = runtime;
        self
    }

    pub(crate) fn reset_graph(&mut self) {
        self.graph = self.initial_graph.clone();
    }

    async fn get_or_insert_function(&mut self) -> anyhow::Result<Rc<Function>> {
        if let Some(function) = &self.runtime {
            return Ok(function.clone());
        }

        let function = Function::create(FunctionConfig {
            listeners: Some(vec![
                Box::new(ConsoleListener),
                Box::new(ZenListener {
                    loader: self.loader.clone(),
                    adapter: self.adapter.clone(),
                }),
            ]),
        })
        .await
        .map_err(|err| anyhow!(err.to_string()))?;
        let rc_function = Rc::new(function);
        self.runtime.replace(rc_function.clone());

        Ok(rc_function)
    }

    pub fn validate(&self) -> Result<(), DecisionGraphValidationError> {
        let input_count = self.input_node_count();
        if input_count != 1 {
            return Err(DecisionGraphValidationError::InvalidInputCount(
                input_count as u32,
            ));
        }

        if is_cyclic_directed(&self.graph) {
            return Err(DecisionGraphValidationError::CyclicGraph);
        }

        Ok(())
    }

    fn input_node_count(&self) -> usize {
        self.graph
            .node_weights()
            .filter(|weight| matches!(weight.kind, DecisionNodeKind::InputNode { content: _ }))
            .count()
    }

    pub async fn evaluate(
        &mut self,
        context: Variable,
    ) -> Result<DecisionGraphResponse, NodeError> {
        let root_start = Instant::now();

        self.validate().map_err(|e| NodeError {
            node_id: "".to_string(),
            source: anyhow!(e),
            trace: None,
        })?;

        if self.iteration >= self.max_depth {
            return Err(NodeError {
                node_id: "".to_string(),
                source: anyhow!(EvaluationError::DepthLimitExceeded),
                trace: None,
            });
        }

        let mut walker = GraphWalker::new(&self.graph);
        let mut node_traces = self.trace.then(|| HashMap::default());

        while let Some(nid) = walker.next(
            &mut self.graph,
            self.trace.then_some(|mut trace: DecisionGraphTrace| {
                if let Some(nt) = &mut node_traces {
                    trace.order = nt.len() as u32;
                    nt.insert(trace.id.clone(), trace);
                };
            }),
        ) {
            if let Some(_) = walker.get_node_data(nid) {
                continue;
            }

            let node = (&self.graph[nid]).clone();
            let start = Instant::now();

            macro_rules! trace {
                ({ $($field:ident: $value:expr),* $(,)? }) => {
                    if let Some(nt) = &mut node_traces {
                        nt.insert(
                            node.id.clone(),
                            DecisionGraphTrace {
                                name: node.name.clone(),
                                id: node.id.clone(),
                                performance: Some(format!("{:.1?}", start.elapsed())),
                                order: nt.len() as u32,
                                $($field: $value,)*
                            }
                        );
                    }
                };
            }

            match &node.kind {
                DecisionNodeKind::InputNode { content } => {
                    trace!({
                        input: Variable::Null,
                        output: context.clone(),
                        trace_data: None,
                    });

                    if let Some(json_schema) = content
                        .schema
                        .as_ref()
                        .map(|s| serde_json::from_str::<Value>(&s).ok())
                        .flatten()
                    {
                        let validator_key = create_validator_cache_key(&json_schema);
                        let validator = self
                            .validator_cache
                            .get_or_insert(validator_key, &json_schema)
                            .await
                            .map_err(|e| NodeError {
                                source: e.into(),
                                node_id: node.id.clone(),
                                trace: error_trace(&node_traces),
                            })?;

                        let context_json = context.to_value();
                        validator.validate(&context_json).map_err(|e| NodeError {
                            source: anyhow!(serde_json::to_value(
                                Into::<Box<EvaluationError>>::into(e)
                            )
                            .unwrap_or_default()),
                            node_id: node.id.clone(),
                            trace: error_trace(&node_traces),
                        })?;
                    }

                    walker.set_node_data(nid, context.clone());
                }
                DecisionNodeKind::OutputNode { content } => {
                    let incoming_data = walker.incoming_node_data(&self.graph, nid, false);

                    trace!({
                        input: incoming_data.clone(),
                        output: Variable::Null,
                        trace_data: None,
                    });

                    if let Some(json_schema) = content
                        .schema
                        .as_ref()
                        .map(|s| serde_json::from_str::<Value>(&s).ok())
                        .flatten()
                    {
                        let validator_key = create_validator_cache_key(&json_schema);
                        let validator = self
                            .validator_cache
                            .get_or_insert(validator_key, &json_schema)
                            .await
                            .map_err(|e| NodeError {
                                source: e.into(),
                                node_id: node.id.clone(),
                                trace: error_trace(&node_traces),
                            })?;

                        let incoming_data_json = incoming_data.to_value();
                        validator
                            .validate(&incoming_data_json)
                            .map_err(|e| NodeError {
                                source: anyhow!(serde_json::to_value(
                                    Into::<Box<EvaluationError>>::into(e)
                                )
                                .unwrap_or_default()),
                                node_id: node.id.clone(),
                                trace: error_trace(&node_traces),
                            })?;
                    }

                    return Ok(DecisionGraphResponse {
                        result: incoming_data,
                        performance: format!("{:.1?}", root_start.elapsed()),
                        trace: node_traces,
                    });
                }
                DecisionNodeKind::SwitchNode { .. } => {
                    let input_data = walker.incoming_node_data(&self.graph, nid, false);

                    walker.set_node_data(nid, input_data);
                }
                DecisionNodeKind::FunctionNode { content } => {
                    let function = self.get_or_insert_function().await.map_err(|e| NodeError {
                        source: e.into(),
                        node_id: node.id.clone(),
                        trace: error_trace(&node_traces),
                    })?;

                    let node_request = NodeRequest {
                        node: node.clone(),
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };
                    let res = match content {
                        FunctionNodeContent::Version2(_) => FunctionHandler::new(
                            function,
                            self.trace,
                            self.iteration,
                            self.max_depth,
                        )
                        .handle(node_request.clone())
                        .await
                        .map_err(|e| {
                            if let Some(detailed_err) = e.downcast_ref::<PartialTraceError>() {
                                trace!({
                                    input: node_request.input.clone(),
                                    output: Variable::Null,
                                    trace_data: detailed_err.trace.clone(),
                                });
                            }

                            NodeError {
                                source: e.into(),
                                node_id: node.id.clone(),
                                trace: error_trace(&node_traces),
                            }
                        })?,
                        FunctionNodeContent::Version1(_) => {
                            let runtime = create_runtime().map_err(|e| NodeError {
                                source: e.into(),
                                node_id: node.id.clone(),
                                trace: error_trace(&node_traces),
                            })?;

                            function_v1::FunctionHandler::new(self.trace, runtime)
                                .handle(node_request.clone())
                                .await
                                .map_err(|e| NodeError {
                                    source: e.into(),
                                    node_id: node.id.clone(),
                                    trace: error_trace(&node_traces),
                                })?
                        }
                    };

                    node_request.input.dot_remove("$nodes");
                    res.output.dot_remove("$nodes");

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::DecisionNode { .. } => {
                    let node_request = NodeRequest {
                        node: node.clone(),
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let res = DecisionHandler::new(
                        self.trace,
                        self.max_depth,
                        self.loader.clone(),
                        self.adapter.clone(),
                        self.runtime.clone(),
                        self.validator_cache.clone(),
                    )
                    .handle(node_request.clone())
                    .await
                    .map_err(|e| NodeError {
                        source: e.into(),
                        node_id: node.id.to_string(),
                        trace: error_trace(&node_traces),
                    })?;

                    node_request.input.dot_remove("$nodes");
                    res.output.dot_remove("$nodes");

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::DecisionTableNode { .. } => {
                    let node_request = NodeRequest {
                        node: node.clone(),
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let res = DecisionTableHandler::new(self.trace)
                        .handle(node_request.clone())
                        .await
                        .map_err(|e| NodeError {
                            node_id: node.id.clone(),
                            source: e.into(),
                            trace: error_trace(&node_traces),
                        })?;

                    node_request.input.dot_remove("$nodes");
                    res.output.dot_remove("$nodes");
                    res.output.dot_remove("$");

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::ExpressionNode { .. } => {
                    let node_request = NodeRequest {
                        node: node.clone(),
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let res = ExpressionHandler::new(self.trace)
                        .handle(node_request.clone())
                        .await
                        .map_err(|e| {
                            if let Some(detailed_err) = e.downcast_ref::<PartialTraceError>() {
                                trace!({
                                    input: node_request.input.clone(),
                                    output: Variable::Null,
                                    trace_data: detailed_err.trace.clone(),
                                });
                            }

                            NodeError {
                                node_id: node.id.clone(),
                                source: e.into(),
                                trace: error_trace(&node_traces),
                            }
                        })?;

                    node_request.input.dot_remove("$nodes");
                    res.output.dot_remove("$nodes");

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
                DecisionNodeKind::CustomNode { .. } => {
                    let node_request = NodeRequest {
                        node: node.clone(),
                        iteration: self.iteration,
                        input: walker.incoming_node_data(&self.graph, nid, true),
                    };

                    let res = self
                        .adapter
                        .handle(CustomNodeRequest::try_from(node_request.clone()).unwrap())
                        .await
                        .map_err(|e| NodeError {
                            node_id: node.id.clone(),
                            source: e.into(),
                            trace: error_trace(&node_traces),
                        })?;

                    node_request.input.dot_remove("$nodes");
                    res.output.dot_remove("$nodes");

                    trace!({
                        input: node_request.input,
                        output: res.output.clone(),
                        trace_data: res.trace_data,
                    });
                    walker.set_node_data(nid, res.output);
                }
            }
        }

        Ok(DecisionGraphResponse {
            result: walker.ending_variables(&self.graph),
            performance: format!("{:.1?}", root_start.elapsed()),
            trace: node_traces,
        })
    }
}

#[derive(Debug, Error)]
pub enum DecisionGraphValidationError {
    #[error("Invalid input node count: {0}")]
    InvalidInputCount(u32),

    #[error("Invalid output node count: {0}")]
    InvalidOutputCount(u32),

    #[error("Cyclic graph detected")]
    CyclicGraph,

    #[error("Missing node")]
    MissingNode(String),
}

impl Serialize for DecisionGraphValidationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(None)?;

        match &self {
            DecisionGraphValidationError::InvalidInputCount(count) => {
                map.serialize_entry("type", "invalidInputCount")?;
                map.serialize_entry("nodeCount", count)?;
            }
            DecisionGraphValidationError::InvalidOutputCount(count) => {
                map.serialize_entry("type", "invalidOutputCount")?;
                map.serialize_entry("nodeCount", count)?;
            }
            DecisionGraphValidationError::MissingNode(node_id) => {
                map.serialize_entry("type", "missingNode")?;
                map.serialize_entry("nodeId", node_id)?;
            }
            DecisionGraphValidationError::CyclicGraph => {
                map.serialize_entry("type", "cyclicGraph")?;
            }
        }

        map.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphResponse {
    pub performance: String,
    pub result: Variable,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<HashMap<String, DecisionGraphTrace>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionGraphTrace {
    pub input: Variable,
    pub output: Variable,
    pub name: String,
    pub id: String,
    pub performance: Option<String>,
    pub trace_data: Option<Value>,
    pub order: u32,
}

pub(crate) fn error_trace(trace: &Option<HashMap<String, DecisionGraphTrace>>) -> Option<Value> {
    trace
        .as_ref()
        .map(|s| serde_json::to_value(s).ok())
        .flatten()
}

fn create_validator_cache_key(content: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

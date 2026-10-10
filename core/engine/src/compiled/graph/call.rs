use crate::compiled::exact::graph::Row;
use super::traced::Traced;
use super::{CompiledGraph, Nesting, Source};
use crate::decision_graph::graph::DecisionGraphResponse;
use crate::loader::LoaderError;
use crate::model::GraphContent;
use crate::nodes::{NodeError, NodeHandlerExtensions, NodeResponse, NodeResult};
use crate::EvaluationError;
use std::cell::{OnceCell, RefCell};
use std::error::Error;
use std::sync::Arc;
use zen_expression::variable::ToVariable;
use zen_expression::Variable;
use zen_types::decision::TransformAttributes;

pub(crate) struct Call<'a> {
    pub(crate) key: &'a str,
    pub(crate) extensions: &'a NodeHandlerExtensions,
    pub(crate) max_depth: u8,
    pub(crate) iteration: u8,
}

pub(crate) enum Load {
    Graph(Arc<GraphContent>),
    Kind(&'static str),
}

pub(crate) type TracedRun = Result<DecisionGraphResponse, Box<EvaluationError>>;

impl Call<'_> {
    pub(crate) async fn load(&self) -> Result<Load, LoaderError> {
        let loader = self.extensions.loader();
        let content = loader.load(self.key).await?;
        let kind = content.kind();
        match content.into_graph_arc() {
            Some(graph) => Ok(Load::Graph(GraphContent::loaded(graph, loader).await)),
            None => Ok(Load::Kind(kind)),
        }
    }

    pub(super) fn compiled(graph: &GraphContent) -> Result<&CompiledGraph, Box<dyn Error>> {
        let plan = graph.compiled_plan.as_deref().ok_or_else(Self::uncompiled)?;
        plan.graph().map_err(|invalid| invalid.source())
    }

    pub(super) fn uncompiled() -> Box<dyn Error> {
        "sub-decision is not compiled".into()
    }

    pub(super) fn kind(key: &str, kind: &str) -> Box<dyn Error> {
        format!("sub-decision '{key}' is a {kind}, expected graph").into()
    }

    pub(super) fn sub_extensions(&self, content: &GraphContent) -> NodeHandlerExtensions {
        NodeHandlerExtensions {
            #[cfg(feature = "driver")]
            compiled_cache: content.compiled_cache.clone(),
            dt_indexes: content.dt_indexes.clone(),
            validator_cache: Arc::new(OnceCell::from(content.validator_cache.clone())),
            ..self.extensions.clone()
        }
    }

    pub(crate) async fn run(
        &self,
        graph: &Arc<GraphContent>,
        input: Variable,
        parent: Option<Variable>,
    ) -> Result<Variable, Box<dyn Error>> {
        let compiled = Self::compiled(graph)?;
        let content = compiled.schemas(graph, self.extensions.loader()).await;
        let extensions = self.sub_extensions(&content);
        let parents = [parent];
        let inputs = [input];
        let nesting = Nesting {
            parents: Some(&parents),
            iteration: self.iteration + 1,
        };
        let state = Box::pin(compiled.run(&content, &extensions, self.max_depth, &Source::Rows(&inputs), 1, false, nesting)).await;
        let error = state.errors.into_iter().next().flatten();
        let result = state.results.into_iter().next().flatten();
        match error {
            Some(error) => Err(error.to_string().into()),
            None => Ok(result.unwrap_or_else(Variable::empty_object)),
        }
    }

    pub(crate) async fn traced(
        &self,
        graph: &Arc<GraphContent>,
        input: Variable,
        parent: Option<Variable>,
    ) -> Result<TracedRun, Box<dyn Error>> {
        let compiled = Self::compiled(graph)?;
        let content = compiled.schemas(graph, self.extensions.loader()).await;
        let extensions = self.sub_extensions(&content);
        let traced = Traced {
            graph: compiled,
            content: &content,
            extensions: &extensions,
            max_depth: self.max_depth,
            iteration: self.iteration + 1,
            parent,
        };
        Ok(Box::pin(traced.evaluate(input)).await)
    }

    pub(crate) async fn node(&self, row: Row<'_>, attributes: &TransformAttributes) -> NodeResult {
        let loaded: RefCell<Option<Arc<GraphContent>>> = RefCell::new(None);
        let loaded = &loaded;
        let id = row.id;
        let nodes = &row.nodes;
        let traced = row.trace;
        row.transform(attributes, |input, _| async move {
            let fault = |source: Box<dyn Error>, trace: Option<Variable>| NodeError {
                node_id: id.clone(),
                trace,
                source,
            };
            let blank = traced.then_some(Variable::Null);
            let cached = loaded.borrow().clone();
            let graph = match cached {
                Some(graph) => graph,
                None => match self.load().await {
                    Err(error) => return Err(fault(error.into(), blank)),
                    Ok(Load::Kind(kind)) => return Err(fault(Self::kind(self.key, kind), blank)),
                    Ok(Load::Graph(graph)) => {
                        *loaded.borrow_mut() = Some(graph.clone());
                        graph
                    }
                },
            };
            if !traced {
                let output = self.run(&graph, input, nodes.clone()).await.map_err(|e| fault(e, None))?;
                return Ok(NodeResponse {
                    output,
                    trace_data: None,
                });
            }
            match self.traced(&graph, input, nodes.clone()).await {
                Err(source) => Err(fault(source, blank)),
                Ok(Ok(response)) => Ok(NodeResponse {
                    output: response.result,
                    trace_data: Some(
                        response
                            .trace
                            .and_then(|trace| trace.into_graph())
                            .as_ref()
                            .map(ToVariable::to_variable)
                            .unwrap_or(Variable::Null),
                    ),
                }),
                Ok(Err(error)) => {
                    let trace = match error.as_ref() {
                        EvaluationError::NodeError { trace, .. } => trace.to_variable(),
                        _ => Variable::Null,
                    };
                    Err(fault(error.to_string().into(), Some(trace)))
                }
            }
        })
        .await
    }
}
